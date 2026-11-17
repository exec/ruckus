//! RPL_ISUPPORT (005) features and IRC-specific case folding.
//!
//! Adapted from Lurk's `isupport/` (<https://github.com/exec/lurk>).
//! Copyright (c) 2026 exec. MIT licensed; see the repository LICENSE.
//! This module performs no I/O and does not depend on wire message types.

use std::collections::BTreeMap;

const MAX_TOKENS: usize = 512;
const MAX_TOKEN_BYTES: usize = 4096;

/// Server nickname/channel comparison rules, independent of Unicode case folding.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CaseMapping {
    /// ASCII letters plus `[\\]^` fold to `{|}~` (the historical default).
    #[default]
    Rfc1459,
    /// RFC1459 without the `^`/`~` equivalence.
    Rfc1459Strict,
    /// Only ASCII letters fold.
    Ascii,
}

impl CaseMapping {
    /// Returns the canonical CASEMAPPING token value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rfc1459 => "rfc1459",
            Self::Rfc1459Strict => "rfc1459-strict",
            Self::Ascii => "ascii",
        }
    }

    /// Folds ASCII protocol characters, leaving all non-ASCII text unchanged.
    pub fn fold(self, text: &str) -> String {
        text.chars()
            .map(|ch| match ch {
                'A'..='Z' => ch.to_ascii_lowercase(),
                '[' if self != Self::Ascii => '{',
                ']' if self != Self::Ascii => '}',
                '\\' if self != Self::Ascii => '|',
                '^' if self == Self::Rfc1459 => '~',
                _ => ch,
            })
            .collect()
    }
}

/// Four CHANMODES groups; membership modes are described separately by PREFIX.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChanModes {
    /// List modes: parameter required both when setting and removing.
    pub a: String,
    /// Scalar modes: parameter required both when setting and removing.
    pub b: String,
    /// Parameter required only when setting.
    pub c: String,
    /// Modes which never take a parameter.
    pub d: String,
}

impl ChanModes {
    /// Whether a known mode requires a parameter; `None` means unknown.
    pub fn takes_parameter(&self, mode: char, setting: bool) -> Option<bool> {
        if self.a.contains(mode) || self.b.contains(mode) {
            Some(true)
        } else if self.c.contains(mode) {
            Some(setting)
        } else if self.d.contains(mode) {
            Some(false)
        } else {
            None
        }
    }
}

/// Accumulated ISUPPORT tokens with historical defaults for absent features.
///
/// At most 512 keys are retained, each input token at most 4096 bytes.
/// Invalid keys and oversized tokens are ignored. Known keys may still be
/// updated or removed at capacity. Merging never mutates older values.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ISupport {
    tokens: BTreeMap<String, String>,
}

impl ISupport {
    /// Parses a batch of `KEY`, `KEY=value`, and `-KEY` tokens.
    pub fn parse(tokens: &[&str]) -> Self {
        Self::default().merge(tokens)
    }

    /// Returns a new feature set with this batch applied; keys are ASCII-insensitive.
    ///
    /// Values decode `\xHH` escapes. Malformed escapes remain literal.
    /// Decoded invalid UTF-8 is replaced with U+FFFD (Rust values are UTF-8).
    pub fn merge(&self, tokens: &[&str]) -> Self {
        let mut next = self.clone();
        for &token in tokens {
            if token.len() > MAX_TOKEN_BYTES {
                continue;
            }
            let (remove, token) = match token.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, token),
            };
            let (key, value) = token.split_once('=').unwrap_or((token, ""));
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
                continue;
            }
            let key = key.to_ascii_uppercase();
            if remove {
                next.tokens.remove(&key);
            } else if next.tokens.contains_key(&key) || next.tokens.len() < MAX_TOKENS {
                next.tokens.insert(key, decode_value(value));
            }
        }
        next
    }

    /// Extracts tokens from numeric 005 parameters, excluding nick and trailer.
    /// The caller must first check that the command is `005`.
    pub fn tokens_from_params<'a>(params: &'a [&'a str]) -> &'a [&'a str] {
        if params.len() < 3 {
            &[]
        } else {
            &params[1..params.len() - 1]
        }
    }

    /// Returns a decoded value, distinguishing absent keys from empty flags.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.tokens
            .get(&key.to_ascii_uppercase())
            .map(String::as_str)
    }

    /// Network display name, or an empty string.
    pub fn network(&self) -> &str {
        self.get("NETWORK").unwrap_or("")
    }

    /// Valid channel prefixes; defaults to `#&`, honoring explicit empty values.
    pub fn chan_types(&self) -> &str {
        self.get("CHANTYPES").unwrap_or("#&")
    }

    /// Status-message target prefixes, or an empty string.
    pub fn status_msg(&self) -> &str {
        self.get("STATUSMSG").unwrap_or("")
    }

    /// Advertised case mapping; absent/unknown values use RFC1459.
    pub fn case_mapping(&self) -> CaseMapping {
        match self
            .get("CASEMAPPING")
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "ascii" => CaseMapping::Ascii,
            "rfc1459-strict" => CaseMapping::Rfc1459Strict,
            _ => CaseMapping::Rfc1459,
        }
    }

    /// Aligned `(modes, symbols)` in descending privilege order.
    ///
    /// Missing/malformed PREFIX defaults to `("ov", "@+")`; explicit empty
    /// means no prefixes. Mismatched ASCII halves truncate to the shorter one
    /// as in Lurk. Non-ASCII/control characters are malformed.
    pub fn prefix(&self) -> (&str, &str) {
        let Some(value) = self.get("PREFIX") else {
            return ("ov", "@+");
        };
        if value.is_empty() {
            return ("", "");
        }
        match value
            .strip_prefix('(')
            .and_then(|rest| rest.split_once(')'))
        {
            Some((modes, symbols))
                if modes.bytes().all(|b| b.is_ascii_alphabetic())
                    && symbols.bytes().all(|b| b.is_ascii_graphic()) =>
            {
                let n = modes.len().min(symbols.len());
                (&modes[..n], &symbols[..n])
            }
            _ => ("ov", "@+"),
        }
    }

    /// Display symbol corresponding to a membership mode.
    pub fn prefix_symbol_for_mode(&self, mode: char) -> Option<char> {
        let (modes, symbols) = self.prefix();
        modes
            .chars()
            .zip(symbols.chars())
            .find_map(|(m, s)| (m == mode).then_some(s))
    }

    /// Membership mode corresponding to a NAMES display prefix.
    pub fn prefix_mode_for_symbol(&self, symbol: char) -> Option<char> {
        let (modes, symbols) = self.prefix();
        modes
            .chars()
            .zip(symbols.chars())
            .find_map(|(m, s)| (s == symbol).then_some(m))
    }

    /// Parses up to four CHANMODES groups; absent/missing groups are empty.
    pub fn chan_modes(&self) -> ChanModes {
        let mut groups = self.get("CHANMODES").unwrap_or("").split(',');
        ChanModes {
            a: groups.next().unwrap_or("").into(),
            b: groups.next().unwrap_or("").into(),
            c: groups.next().unwrap_or("").into(),
            d: groups.next().unwrap_or("").into(),
        }
    }

    /// Maximum nickname length, clamped to 512; malformed/absent is `None`.
    pub fn nick_len(&self) -> Option<usize> {
        bounded_integer(self.get("NICKLEN")?, 512)
    }

    /// Maximum channel length, clamped to 512; malformed/absent is `None`.
    pub fn channel_len(&self) -> Option<usize> {
        bounded_integer(self.get("CHANNELLEN")?, 512)
    }

    /// Maximum topic length, clamped to 8192; malformed/absent is `None`.
    pub fn topic_len(&self) -> Option<usize> {
        bounded_integer(self.get("TOPICLEN")?, 8192)
    }

    /// Per-command TARGMAX, clamped to 1000. Unlimited/absent/malformed is `None`.
    pub fn targ_max(&self, command: &str) -> Option<usize> {
        for entry in self.get("TARGMAX")?.split(',') {
            let (name, value) = entry.split_once(':').unwrap_or((entry, ""));
            if name.eq_ignore_ascii_case(command) {
                return bounded_integer(value, 1000);
            }
        }
        None
    }
}

fn bounded_integer(value: &str, limit: usize) -> Option<usize> {
    let value = value.strip_prefix('+').unwrap_or(value);
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(value.bytes().fold(0usize, |n, b| {
        n.saturating_mul(10)
            .saturating_add(usize::from(b - b'0'))
            .min(limit)
    }))
}

fn decode_value(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() && matches!(bytes[i + 1], b'x' | b'X') {
            let hi = char::from(bytes[i + 2]).to_digit(16);
            let lo = char::from(bytes[i + 3]).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                decoded.push((hi * 16 + lo) as u8);
                i += 4;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings_match_go_reference() {
        for (token, mapping, expected) in [
            ("ascii", CaseMapping::Ascii, "abc[]\\^ä"),
            ("rfc1459-strict", CaseMapping::Rfc1459Strict, "abc{}|^ä"),
            ("rfc1459", CaseMapping::Rfc1459, "abc{}|~ä"),
            ("unknown", CaseMapping::Rfc1459, "abc{}|~ä"),
        ] {
            let s = ISupport::parse(&[&format!("CASEMAPPING={token}")]);
            assert_eq!(s.case_mapping(), mapping);
            assert_eq!(mapping.fold("ABC[]\\^ä"), expected);
            assert_eq!(mapping.fold(expected), expected);
        }
        assert_eq!(CaseMapping::default().as_str(), "rfc1459");
    }

    #[test]
    fn tokens_accumulate_without_mutation() {
        let s = ISupport::parse(&["network=Libera", "EXCEPTS", "NICKLEN=30"]);
        let t = s.merge(&["NETWORK=Ergo", "-NICKLEN=ignored", "INVEX"]);
        assert_eq!(s.network(), "Libera");
        assert_eq!(s.nick_len(), Some(30));
        assert_eq!(t.network(), "Ergo");
        assert_eq!(t.nick_len(), None);
        assert_eq!(t.get("excepts"), Some(""));
        assert_eq!(t.get("INVEX"), Some(""));
        assert_eq!(t.get("MISSING"), None);
        assert_eq!(ISupport::default().chan_types(), "#&");
        assert_eq!(ISupport::parse(&["CHANTYPES="]).chan_types(), "");
        assert_eq!(ISupport::parse(&["STATUSMSG=@+"]).status_msg(), "@+");
    }

    #[test]
    fn prefix_and_channel_modes() {
        for (token, expected) in [
            ("PREFIX=(qaohv)~&@%+", ("qaohv", "~&@%+")),
            ("PREFIX=(ov)@", ("o", "@")),
            ("PREFIX=", ("", "")),
            ("PREFIX=broken", ("ov", "@+")),
            ("PREFIX=(ov)é", ("ov", "@+")),
            ("PREFIX=(ov", ("ov", "@+")),
        ] {
            assert_eq!(ISupport::parse(&[token]).prefix(), expected);
        }
        let s = ISupport::parse(&["CHANMODES=beI,k,l,imnpst,ignored"]);
        assert_eq!(s.prefix_symbol_for_mode('o'), Some('@'));
        assert_eq!(s.prefix_mode_for_symbol('+'), Some('v'));
        assert_eq!(s.prefix_mode_for_symbol('!'), None);
        for (mode, setting, expected) in [
            ('b', false, Some(true)),
            ('k', false, Some(true)),
            ('l', false, Some(false)),
            ('l', true, Some(true)),
            ('n', true, Some(false)),
            ('?', true, None),
        ] {
            assert_eq!(s.chan_modes().takes_parameter(mode, setting), expected);
        }
        assert_eq!(ISupport::default().chan_modes(), ChanModes::default());
    }

    #[test]
    fn integer_limits_and_targmax() {
        for (value, expected) in [
            ("32", Some(32)),
            ("0", Some(0)),
            ("-1", None),
            ("junk", None),
            ("", None),
            ("+20", Some(20)),
            ("9999999999999999999999999999999999999", Some(512)),
        ] {
            assert_eq!(
                ISupport::parse(&[&format!("NICKLEN={value}")]).nick_len(),
                expected
            );
        }
        let s = ISupport::parse(&[
            "CHANNELLEN=2000",
            "TOPICLEN=99999",
            "TARGMAX=PRIVMSG:4,NOTICE:99999,KICK:,JOIN:-1",
        ]);
        assert_eq!(s.channel_len(), Some(512));
        assert_eq!(s.topic_len(), Some(8192));
        assert_eq!(s.targ_max("privmsg"), Some(4));
        assert_eq!(s.targ_max("NOTICE"), Some(1000));
        assert_eq!(s.targ_max("KICK"), None);
        assert_eq!(s.targ_max("JOIN"), None);
        assert_eq!(s.targ_max("WHO"), None);
    }

    #[test]
    fn escaping_and_params() {
        let s = ISupport::parse(&[
            r"NETWORK=Hello\x20world\X5c\xZZ",
            r"UTF8=\xC3\xA9",
            r"INVALID=\xff",
            "",
            "=bad",
            "BAD KEY=bad",
        ]);
        assert_eq!(s.network(), r"Hello world\\xZZ");
        assert_eq!(s.get("UTF8"), Some("é"));
        assert_eq!(s.get("INVALID"), Some("�"));
        assert_eq!(s.get(""), None);
        assert_eq!(
            ISupport::tokens_from_params(&["nick", "A", "B", "supported"]),
            ["A", "B"]
        );
        assert!(ISupport::tokens_from_params(&["nick", "supported"]).is_empty());
    }

    #[test]
    fn flood_bounds_allow_updates_and_removals() {
        let tokens: Vec<_> = (0..600).map(|i| format!("KEY{i}=old")).collect();
        let refs: Vec<_> = tokens.iter().map(String::as_str).collect();
        let s = ISupport::parse(&refs);
        assert_eq!(s.tokens.len(), MAX_TOKENS);
        let s = s.merge(&["KEY0=new", "-KEY1", "NEW=yes", "EXTRA=no"]);
        assert_eq!(s.get("KEY0"), Some("new"));
        assert_eq!(s.get("NEW"), Some("yes"));
        assert_eq!(s.get("EXTRA"), None);
        assert_eq!(s.get("KEY1"), None);
        let huge = format!("KEY0={}", "x".repeat(MAX_TOKEN_BYTES));
        assert_eq!(s.merge(&[&huge]).get("KEY0"), Some("new"));
    }
}
