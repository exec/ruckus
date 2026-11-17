//! IRC wire messages and IRCv3 tags, independent of transport.
//!
//! Ported from Lurk's `irc/` package (<https://github.com/exec/lurk>).
//! Parsing preserves command case and tolerates repeated spaces and nonstandard
//! command tokens. Serialization validates framing, not command semantics.
//! Transports must check [`validate_wire_budgets`] before parsing or sending.
//! This API accepts UTF-8; decoding other network bytes belongs to transport.
//! Remote text still needs sanitization before terminal rendering.
//!
//! # License
//!
//! MIT License
//!
//! Copyright (c) 2026 exec
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use std::collections::BTreeMap;
use std::fmt;

/// Maximum tag segment bytes, including leading `@` and following space.
pub const MAX_LEN_TAGS: usize = 8191;
/// Maximum combined tag data bytes, excluding `@` and following space.
pub const MAX_LEN_TAG_DATA: usize = MAX_LEN_TAGS - 2;
/// Maximum client-added tag data bytes, excluding `@` and following space.
pub const MAX_LEN_CLIENT_TAG_DATA: usize = 4094;
/// Maximum server-added tag data bytes, excluding `@` and following space.
pub const MAX_LEN_SERVER_TAG_DATA: usize = 4094;
/// Maximum non-tag message bytes, including terminating CRLF.
pub const MAX_LEN_MESSAGE: usize = 512;
/// Maximum parameters retained by the parser (not the RFC's usual 15).
pub const MAX_PARAM_COUNT: usize = 32;
/// Maximum distinct tags retained by the parser.
pub const MAX_TAG_COUNT: usize = 64;

/// A framing, validation, or wire-budget failure.
///
/// Errors never contain field contents, which may be credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Error {
    /// The line or message has no command.
    EmptyMessage,
    /// NUL, CR, or LF occurs where it cannot be escaped.
    IllegalByte,
    /// A command has a space or non-ASCII-alphanumeric start.
    InvalidCommand,
    /// A source contains a space or is explicitly present but empty.
    InvalidSource,
    /// A non-final parameter is empty, colon-prefixed, or space-containing.
    InvalidParameter,
    /// A tag key is empty or contains a wire delimiter.
    InvalidTagKey,
    /// The tag segment exceeds [`MAX_LEN_TAGS`].
    TagsTooLong,
    /// The non-tag message, including CRLF, exceeds [`MAX_LEN_MESSAGE`].
    MessageTooLong,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyMessage => "irc: message has no command",
            Self::IllegalByte => "irc: illegal NUL, CR, or LF",
            Self::InvalidCommand => "irc: invalid command framing",
            Self::InvalidSource => "irc: invalid source framing",
            Self::InvalidParameter => "irc: invalid non-final parameter",
            Self::InvalidTagKey => "irc: invalid tag key",
            Self::TagsTooLong => "irc: tag segment exceeds byte budget",
            Self::MessageTooLong => "irc: message exceeds byte budget",
        })
    }
}

impl std::error::Error for Error {}

/// A single IRC message with decoded tags and parameters.
///
/// The final parameter's wire `:` marker is not stored. Bare tags and empty
/// values both map to `""`; use `tags.contains_key` to test presence.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Message {
    /// IRCv3 tags without `@`, with values unescaped; `+` prefixes are retained.
    pub tags: BTreeMap<String, String>,
    /// Server name or `nick!user@host` prefix without leading `:`.
    pub source: Option<String>,
    /// Command name or numeric reply, preserved verbatim.
    pub command: String,
    /// Parameters in wire order, including decoded trailing text.
    pub params: Vec<String>,
}

impl Message {
    /// Constructs an untagged, sourceless message; validation is deferred to
    /// [`Self::serialize`].
    pub fn new(command: impl Into<String>, params: Vec<String>) -> Self {
        Self {
            command: command.into(),
            params,
            ..Self::default()
        }
    }

    /// Parses a line, accepting optional CRLF, LF, or CR termination.
    ///
    /// Only one terminator is removed; remaining NUL/CR/LF are rejected.
    /// Repeated ASCII spaces are skipped. Empty tag keys/entries are ignored.
    /// Duplicate tags use the last value until [`MAX_TAG_COUNT`] distinct tags
    /// have been stored, then the rest are ignored. Only the first
    /// [`MAX_PARAM_COUNT`] parameters are retained, matching Lurk.
    ///
    /// Input command syntax is tolerant. Byte budgets are not enforced; call
    /// [`validate_wire_budgets`] on the original line before parsing, because
    /// parsing can discard excess fields.
    pub fn parse(line: &str) -> Result<Self, Error> {
        let mut line = strip_terminator(line);
        check_field(line)?;
        let mut message = Self::default();
        if let Some(tags) = line.strip_prefix('@') {
            let (segment, rest) = tags.split_once(' ').ok_or(Error::EmptyMessage)?;
            for entry in segment.split(';') {
                if message.tags.len() >= MAX_TAG_COUNT {
                    break;
                }
                let (key, value) = entry.split_once('=').unwrap_or((entry, ""));
                if !key.is_empty() {
                    message.tags.insert(key.into(), unescape_tag_value(value));
                }
            }
            line = rest;
        }
        line = line.trim_start_matches(' ');
        if let Some(source) = line.strip_prefix(':') {
            let (source, rest) = source.split_once(' ').ok_or(Error::EmptyMessage)?;
            // Match Lurk's absent/empty source equivalence.
            if !source.is_empty() {
                message.source = Some(source.into());
            }
            line = rest;
        }
        line = line.trim_start_matches(' ');
        let (command, rest) = cut_space(line);
        if command.is_empty() {
            return Err(Error::EmptyMessage);
        }
        message.command = command.into();
        line = rest;
        while message.params.len() < MAX_PARAM_COUNT {
            line = line.trim_start_matches(' ');
            if line.is_empty() {
                break;
            }
            if let Some(trailing) = line.strip_prefix(':') {
                message.params.push(trailing.into());
                break;
            }
            let (param, rest) = cut_space(line);
            message.params.push(param.into());
            line = rest;
        }
        Ok(message)
    }

    /// Serializes one wire line including terminating CRLF.
    ///
    /// Tags are emitted in sorted key order; values escape spaces, semicolons,
    /// backslashes, CR and LF. NUL is rejected everywhere. Non-tag fields also
    /// reject CR/LF. Source, command, tag keys, and non-final parameters are
    /// checked for ambiguous framing. A command must start with an ASCII letter
    /// or digit; its remaining characters are not restricted to RFC grammar.
    /// The final parameter receives `:` if empty, colon-prefixed, or containing
    /// a space. An explicit empty source is rejected (use `None` instead).
    ///
    /// Counts and lengths are not enforced on output; call
    /// [`validate_wire_budgets`] before sending. Messages above the parser's
    /// count caps cannot round-trip losslessly.
    pub fn serialize(&self) -> Result<String, Error> {
        if self.command.is_empty() {
            return Err(Error::EmptyMessage);
        }
        check_field(&self.command)?;
        if !self.command.as_bytes()[0].is_ascii_alphanumeric() || self.command.contains(' ') {
            return Err(Error::InvalidCommand);
        }
        let mut wire = String::new();
        if !self.tags.is_empty() {
            wire.push('@');
            for (index, (key, value)) in self.tags.iter().enumerate() {
                if key.is_empty() || key.contains([' ', ';', '=', '\r', '\n', '\0']) {
                    return Err(Error::InvalidTagKey);
                }
                if value.contains('\0') {
                    return Err(Error::IllegalByte);
                }
                if index != 0 {
                    wire.push(';');
                }
                wire.push_str(key);
                if !value.is_empty() {
                    wire.push('=');
                    escape_tag_value(value, &mut wire);
                }
            }
            wire.push(' ');
        }
        if let Some(source) = &self.source {
            check_field(source)?;
            if source.is_empty() || source.contains(' ') {
                return Err(Error::InvalidSource);
            }
            wire.push(':');
            wire.push_str(source);
            wire.push(' ');
        }
        wire.push_str(&self.command);
        for (index, param) in self.params.iter().enumerate() {
            check_field(param)?;
            let trailing = param.is_empty() || param.contains(' ') || param.starts_with(':');
            if index + 1 != self.params.len() && trailing {
                return Err(Error::InvalidParameter);
            }
            wire.push(' ');
            if trailing {
                wire.push(':');
            }
            wire.push_str(param);
        }
        wire.push_str("\r\n");
        Ok(wire)
    }

    /// Returns a parameter, or `""` when its index is out of bounds.
    pub fn param(&self, index: usize) -> &str {
        self.params.get(index).map_or("", String::as_str)
    }

    /// Returns the source before the first `!` or `@`, or `""` if absent.
    pub fn nick(&self) -> &str {
        self.source
            .as_deref()
            .unwrap_or("")
            .split(['!', '@'])
            .next()
            .unwrap_or("")
    }

    /// Returns the source ident between `!` and `@`, or `""` if absent.
    pub fn user(&self) -> &str {
        let source = self.source.as_deref().unwrap_or("");
        let before_host = source.split_once('@').map_or("", |(before, _)| before);
        before_host.split_once('!').map_or("", |(_, user)| user)
    }

    /// Returns the source after its first `@`, or `""` if absent.
    pub fn host(&self) -> &str {
        self.source
            .as_deref()
            .unwrap_or("")
            .split_once('@')
            .map_or("", |(_, host)| host)
    }
}

/// Checks independent tag and message byte budgets on an original wire line.
///
/// Accepts optional CRLF, LF, or CR but always charges two bytes for CRLF.
/// Tags include `@` and following space in their 8191-byte allowance; the body
/// has its own 512-byte allowance including CRLF. Escaped tag bytes, duplicate
/// fields, surplus spaces and fields discarded by parsing all count.
///
/// This is not command validation. It rejects embedded NUL/CR/LF and tag-only
/// lines, but does not enforce the stricter 4094-byte allowance for client-added
/// or server-added tag data. A client adding all outbound tags must separately
/// restrict tag data to [`MAX_LEN_CLIENT_TAG_DATA`] (4096 bytes with `@`/space).
pub fn validate_wire_budgets(line: &str) -> Result<(), Error> {
    let line = strip_terminator(line);
    check_field(line)?;
    let body = if line.starts_with('@') {
        let (tags, body) = line.split_once(' ').ok_or(Error::EmptyMessage)?;
        // tags includes '@'; charge the separating space as well.
        if tags.len() >= MAX_LEN_TAGS {
            return Err(Error::TagsTooLong);
        }
        body
    } else {
        line
    };
    if body.len() > MAX_LEN_MESSAGE - 2 {
        return Err(Error::MessageTooLong);
    }
    Ok(())
}

fn strip_terminator(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn cut_space(value: &str) -> (&str, &str) {
    value.split_once(' ').unwrap_or((value, ""))
}

fn check_field(value: &str) -> Result<(), Error> {
    if value.contains(['\0', '\r', '\n']) {
        Err(Error::IllegalByte)
    } else {
        Ok(())
    }
}

fn unescape_tag_value(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
        } else if let Some(escaped) = chars.next() {
            decoded.push(match escaped {
                ':' => ';',
                's' => ' ',
                'r' => '\r',
                'n' => '\n',
                other => other,
            });
        }
    }
    decoded
}

fn escape_tag_value(value: &str, wire: &mut String) {
    for ch in value.chars() {
        match ch {
            ';' => wire.push_str("\\:"),
            ' ' => wire.push_str("\\s"),
            '\\' => wire.push_str("\\\\"),
            '\r' => wire.push_str("\\r"),
            '\n' => wire.push_str("\\n"),
            other => wire.push(other),
        }
    }
}

/// Canonical command names and CAP subcommands; received case is not normalized.
pub mod commands {
    macro_rules! commands {
        ($($name:ident = $value:literal),* $(,)?) => {$(
            #[doc = concat!("IRC command or CAP subcommand `", $value, "`.")]
            pub const $name: &str = $value;
        )*};
    }
    commands! {
        CAP = "CAP", AUTHENTICATE = "AUTHENTICATE", PASS = "PASS",
        NICK = "NICK", USER = "USER", PING = "PING", PONG = "PONG",
        PRIVMSG = "PRIVMSG", NOTICE = "NOTICE", JOIN = "JOIN", PART = "PART",
        QUIT = "QUIT", MODE = "MODE", NAMES = "NAMES", LIST = "LIST",
        WHOIS = "WHOIS", KICK = "KICK", INVITE = "INVITE", TOPIC = "TOPIC",
        BATCH = "BATCH", TAGMSG = "TAGMSG", ACCOUNT = "ACCOUNT", AWAY = "AWAY",
        CHGHOST = "CHGHOST", SETNAME = "SETNAME", CHATHISTORY = "CHATHISTORY",
        MONITOR = "MONITOR", FAIL = "FAIL", WARN = "WARN", NOTE = "NOTE",
        CAP_LS = "LS", CAP_REQ = "REQ", CAP_ACK = "ACK", CAP_NAK = "NAK",
        CAP_NEW = "NEW", CAP_DEL = "DEL", CAP_END = "END", CAP_LIST = "LIST",
    }
}

/// Common numeric replies as three-character command strings.
pub mod numerics {
    macro_rules! numerics {
        ($($name:ident = $value:literal),* $(,)?) => {$(
            #[doc = concat!("IRC numeric `", $value, "` (`", stringify!($name), "`).")]
            pub const $name: &str = $value;
        )*};
    }
    numerics! {
        RPL_WELCOME = "001", RPL_YOURHOST = "002", RPL_CREATED = "003",
        RPL_MYINFO = "004", RPL_ISUPPORT = "005", RPL_NOTOPIC = "331",
        RPL_TOPIC = "332", RPL_TOPICWHOTIME = "333", RPL_NAMREPLY = "353",
        RPL_ENDOFNAMES = "366", RPL_LISTSTART = "321", RPL_LIST = "322",
        RPL_LISTEND = "323", RPL_AWAY = "301", RPL_WHOISUSER = "311",
        RPL_WHOISSERVER = "312", RPL_WHOISOPERATOR = "313", RPL_WHOISIDLE = "317",
        RPL_ENDOFWHOIS = "318", RPL_WHOISCHANNELS = "319", RPL_WHOISACCOUNT = "330",
        RPL_WHOISACTUALLY = "338", RPL_WHOISSECURE = "671", RPL_MOTDSTART = "375",
        RPL_MOTD = "372", RPL_ENDOFMOTD = "376", ERR_NOMOTD = "422",
        ERR_NONICKNAMEGIVEN = "431", ERR_ERRONEUSNICKNAME = "432",
        ERR_NICKNAMEINUSE = "433", ERR_NOTREGISTERED = "451",
        ERR_NEEDMOREPARAMS = "461", ERR_ALREADYREGISTERED = "462",
        ERR_INPUTTOOLONG = "417", RPL_MONONLINE = "730", RPL_MONOFFLINE = "731",
        RPL_MONLIST = "732", RPL_ENDOFMONLIST = "733", ERR_MONLISTFULL = "734",
        RPL_LOGGEDIN = "900", RPL_LOGGEDOUT = "901", ERR_NICKLOCKED = "902",
        RPL_SASLSUCCESS = "903", ERR_SASLFAIL = "904", ERR_SASLTOOLONG = "905",
        ERR_SASLABORTED = "906", ERR_SASLALREADY = "907", RPL_SASLMECHS = "908",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(command: &str, params: &[&str]) -> Message {
        Message::new(command, params.iter().map(|s| (*s).into()).collect())
    }

    #[test]
    fn parse_commands_sources_and_params() {
        for (line, source, command, params) in [
            ("PING", None, "PING", vec![]),
            ("JOIN #chan key", None, "JOIN", vec!["#chan", "key"]),
            (
                "PRIVMSG #ch :Hello there world",
                None,
                "PRIVMSG",
                vec!["#ch", "Hello there world"],
            ),
            (
                ":nick!u@h PRIVMSG #ch :Hello",
                Some("nick!u@h"),
                "PRIVMSG",
                vec!["#ch", "Hello"],
            ),
            (
                ":server 001 nick :Welcome",
                Some("server"),
                "001",
                vec!["nick", "Welcome"],
            ),
            ("PRIVMSG #ch :", None, "PRIVMSG", vec!["#ch", ""]),
            (
                "PRIVMSG #ch ::leading-colon",
                None,
                "PRIVMSG",
                vec!["#ch", ":leading-colon"],
            ),
            (":src   CMD   a    b", Some("src"), "CMD", vec!["a", "b"]),
            ("   CMD   a :  b  ", None, "CMD", vec!["a", "  b  "]),
            ("privmsg #x :hi", None, "privmsg", vec!["#x", "hi"]),
            (": CMD", None, "CMD", vec![]),
            ("CMD a\tb", None, "CMD", vec!["a\tb"]),
        ] {
            let mut expected = message(command, &params);
            expected.source = source.map(String::from);
            assert_eq!(Message::parse(line), Ok(expected), "{line:?}");
        }
        for suffix in ["", "\r\n", "\n", "\r"] {
            assert_eq!(
                Message::parse(&format!("PING token{suffix}")),
                Ok(message("PING", &["token"]))
            );
        }
    }

    #[test]
    fn parse_empty_and_injected_lines() {
        for line in ["", " ", "@a=b", ":nick!u@h", "@a=b :src", "@a=b ", ":src  "] {
            assert_eq!(Message::parse(line), Err(Error::EmptyMessage), "{line:?}");
        }
        for line in [
            "PRIVMSG #ch :hel\0lo",
            "PRIVMSG #ch :hi\nQUIT",
            "PRIVMSG #ch :hi\rthere",
            "PING token\r\r\n",
            "PING\n\n",
            "PING\n\r",
            "@a=\0 PING",
        ] {
            assert_eq!(Message::parse(line), Err(Error::IllegalByte), "{line:?}");
        }
    }

    #[test]
    fn tags_parse_empty_duplicate_vendor_and_client_keys() {
        let m = Message::parse(
            "@aaa=bbb;ccc;example.com/ddd=eee;+typing=active;a;b=;a=last;;=ignored; CMD",
        )
        .unwrap();
        assert_eq!(
            m.tags,
            BTreeMap::from([
                ("aaa".into(), "bbb".into()),
                ("ccc".into(), "".into()),
                ("example.com/ddd".into(), "eee".into()),
                ("+typing".into(), "active".into()),
                ("a".into(), "last".into()),
                ("b".into(), "".into()),
            ])
        );
        assert!(m.tags.contains_key("ccc"));
        assert!(!m.tags.contains_key("missing"));
        assert!(Message::parse("@ CMD").unwrap().tags.is_empty());
    }

    #[test]
    fn tags_escape_and_unescape() {
        for (raw, decoded) in [
            ("plain", "plain"),
            (r"a\:b", "a;b"),
            (r"a\sb", "a b"),
            (r"a\\b", r"a\b"),
            (r"a\rb", "a\rb"),
            (r"a\nb", "a\nb"),
            (r"\s\s", "  "),
            (r"\x", "x"),
            (r"a\qb", "aqb"),
            ("abc\\", "abc"),
            ("\\", ""),
            (r"\\s", r"\s"),
            (r"\\:", r"\:"),
            (r"\\\\", r"\\"),
            (r"\r\n", "\r\n"),
            (r"\é", "é"),
            (r"a\sb\:c\\d", r"a b;c\d"),
        ] {
            assert_eq!(unescape_tag_value(raw), decoded, "{raw:?}");
        }
        for raw in [
            "",
            "plain",
            ";",
            " ",
            "\\",
            "\r",
            "\n",
            "all five ; \\ \r \n together",
            "é 🦀",
        ] {
            let mut encoded = String::new();
            escape_tag_value(raw, &mut encoded);
            assert_eq!(unescape_tag_value(&encoded), raw);
            let mut m = message("CMD", &[]);
            m.tags.insert("msg".into(), raw.into());
            assert_eq!(Message::parse(&m.serialize().unwrap()).unwrap(), m);
        }
    }

    #[test]
    fn serialize_canonical_lines() {
        for (m, expected) in [
            (message("PING", &["token"]), "PING token\r\n"),
            (
                message("PRIVMSG", &["#ch", "Hello there"]),
                "PRIVMSG #ch :Hello there\r\n",
            ),
            (message("PRIVMSG", &["#ch", ""]), "PRIVMSG #ch :\r\n"),
            (message("PRIVMSG", &["#ch", ":-)"]), "PRIVMSG #ch ::-)\r\n"),
            (message("JOIN", &["#chan", "key"]), "JOIN #chan key\r\n"),
            (message("001", &["me", ":-)"]), "001 me ::-)\r\n"),
        ] {
            assert_eq!(m.serialize().unwrap(), expected);
            assert_eq!(Message::parse(expected).unwrap(), m);
        }
        let mut m = message("CMD", &[]);
        m.source = Some("nick!u@h".into());
        m.tags.insert("z".into(), "".into());
        m.tags.insert("msg".into(), "a b;c\\d\r\n".into());
        assert_eq!(
            m.serialize().unwrap(),
            "@msg=a\\sb\\:c\\\\d\\r\\n;z :nick!u@h CMD\r\n"
        );
        assert_eq!(Message::parse(&m.serialize().unwrap()).unwrap(), m);
    }

    #[test]
    fn serialize_rejects_injection_and_misframing() {
        assert_eq!(Message::default().serialize(), Err(Error::EmptyMessage));
        for command in [":", "@x", "-bad", "a b", "é"] {
            assert_eq!(
                message(command, &[]).serialize(),
                Err(Error::InvalidCommand)
            );
        }
        for bad in ["a\rb", "a\nb", "a\0b"] {
            assert_eq!(message(bad, &[]).serialize(), Err(Error::IllegalByte));
            assert_eq!(message("CMD", &[bad]).serialize(), Err(Error::IllegalByte));
            let mut m = message("CMD", &[]);
            m.source = Some(bad.into());
            assert_eq!(m.serialize(), Err(Error::IllegalByte));
        }
        for bad in ["", "a b", ":x"] {
            assert_eq!(
                message("CMD", &[bad, "last"]).serialize(),
                Err(Error::InvalidParameter)
            );
        }
        for bad in ["", "a b", "a;b", "a=b", "a\rb", "a\nb", "a\0b"] {
            let mut m = message("CMD", &[]);
            m.tags.insert(bad.into(), "v".into());
            assert_eq!(m.serialize(), Err(Error::InvalidTagKey));
        }
        let mut m = message("CMD", &[]);
        m.tags.insert("secret".into(), "password\0".into());
        let error = m.serialize().unwrap_err();
        assert_eq!(error, Error::IllegalByte);
        assert!(!error.to_string().contains("password"));
        for bad in ["", "a b"] {
            m.tags.clear();
            m.source = Some(bad.into());
            assert_eq!(m.serialize(), Err(Error::InvalidSource));
        }
    }

    #[test]
    fn parser_count_caps_and_duplicate_at_capacity() {
        let line = format!("CMD{} :discarded", " a".repeat(MAX_PARAM_COUNT + 20));
        assert_eq!(
            Message::parse(&line).unwrap().params,
            vec!["a"; MAX_PARAM_COUNT]
        );
        let tags = (0..MAX_TAG_COUNT + 20)
            .map(|i| format!("k{i}=v"))
            .collect::<Vec<_>>()
            .join(";");
        let m = Message::parse(&format!("@{tags} CMD")).unwrap();
        assert_eq!(m.tags.len(), MAX_TAG_COUNT);
        for i in 0..MAX_TAG_COUNT {
            assert_eq!(m.tags[&format!("k{i}")], "v");
        }
        let first = (0..MAX_TAG_COUNT)
            .map(|i| format!("k{i}"))
            .collect::<Vec<_>>()
            .join(";");
        let m = Message::parse(&format!("@{first};k0=ignored CMD")).unwrap();
        assert_eq!(m.tags["k0"], "");
    }

    #[test]
    fn independent_byte_budgets_and_terminators() {
        let body = format!("CMD :{}", "x".repeat(MAX_LEN_MESSAGE - 7));
        assert_eq!(body.len(), 510);
        for suffix in ["", "\r\n", "\r", "\n"] {
            assert_eq!(validate_wire_budgets(&format!("{body}{suffix}")), Ok(()));
            assert_eq!(
                validate_wire_budgets(&format!("{body}x{suffix}")),
                Err(Error::MessageTooLong)
            );
        }
        let tags = format!("@k={} ", "x".repeat(MAX_LEN_TAG_DATA - 2));
        assert_eq!(tags.len(), MAX_LEN_TAGS);
        assert_eq!(validate_wire_budgets(&format!("{tags}{body}\r\n")), Ok(()));
        assert_eq!(
            validate_wire_budgets(&format!("@x{tags}{body}")),
            Err(Error::TagsTooLong)
        );
        let unicode = format!("CMD :{}", "é".repeat(253));
        assert_eq!(validate_wire_budgets(&unicode), Err(Error::MessageTooLong));
        assert_eq!(validate_wire_budgets("CMD\nQUIT"), Err(Error::IllegalByte));
        assert_eq!(validate_wire_budgets("@tag"), Err(Error::EmptyMessage));
        assert!(Message::parse(&format!("{body}x")).is_ok());
        assert!(message("CMD", &[&"x".repeat(600)]).serialize().is_ok());
        let mut m = message("CMD", &[]);
        m.tags.insert("k".into(), " ".repeat(4094));
        assert_eq!(
            validate_wire_budgets(&m.serialize().unwrap()),
            Err(Error::TagsTooLong)
        );
    }

    #[test]
    fn source_helpers_and_independent_clone() {
        for (source, nick, user, host) in [
            ("nick!user@host", "nick", "user", "host"),
            ("nick@host", "nick", "", "host"),
            ("nick!user", "nick", "", ""),
            ("server.example.net", "server.example.net", "", ""),
            ("", "", "", ""),
        ] {
            let mut m = message("CMD", &["a", "b"]);
            m.source = Some(source.into());
            assert_eq!((m.nick(), m.user(), m.host()), (nick, user, host));
            assert_eq!(m.param(0), "a");
            assert_eq!(m.param(usize::MAX), "");
        }
        let mut m = message("CMD", &["a"]);
        m.tags.insert("time".into(), "original".into());
        let mut cloned = m.clone();
        cloned.tags.insert("time".into(), "changed".into());
        cloned.params[0] = "changed".into();
        assert_eq!(m.tags["time"], "original");
        assert_eq!(m.params[0], "a");
        assert_eq!(Message::default().nick(), "");
    }

    #[test]
    fn exhaustive_short_hostile_inputs_do_not_panic_or_reframe() {
        let alphabet = [
            "A", "0", " ", ":", "@", ";", "=", "\\", "\r", "\n", "\0", "é",
        ];
        for a in alphabet {
            for b in alphabet {
                for c in alphabet {
                    for d in alphabet {
                        let input = format!("{a}{b}{c}{d}");
                        let Ok(m) = Message::parse(&input) else {
                            continue;
                        };
                        let Ok(wire) = m.serialize() else {
                            continue;
                        };
                        assert_eq!(Message::parse(&wire).unwrap(), m, "{input:?}");
                    }
                }
            }
        }
    }
}
