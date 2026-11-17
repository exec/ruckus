//! Validated, synchronous IRC action builders, independent of client state.
//!
//! Adapted from Lurk's `client/actions.go` (<https://github.com/exec/lurk>).
//! Callers own capability checks, ISUPPORT limits, authentication, delivery and
//! state updates. Returned messages are untagged and have no source prefix.
//! Mutating them afterwards requires revalidation before sending.
//! Returned messages can contain channel keys or private text: never log them,
//! including through `Message`'s unredacted Debug implementation.
//!
//! # License
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

use crate::irc::{self, Message};
use std::fmt;

/// Maximum bytes in one logical text input before splitting.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Maximum messages produced by one logical text input.
pub const MAX_PARTS: usize = 256;

/// Builder failure; errors never echo field contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// Empty/invalid atom, mismatched arguments, or NUL/CR/LF injection.
    InvalidParameter,
    /// More than the conventional IRC limit of 15 parameters.
    TooManyParameters,
    /// One unsplittable command exceeds the 512-byte wire limit.
    TooLong,
    /// Relay prefix and framing leave insufficient space for a UTF-8 character.
    InsufficientBudget,
    /// Logical input or number of split messages exceeds its bound.
    TooLarge,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidParameter => "invalid IRC action parameter",
            Self::TooManyParameters => "too many IRC action parameters",
            Self::TooLong => "IRC action exceeds wire budget",
            Self::InsufficientBudget => "insufficient IRC relay budget",
            Self::TooLarge => "IRC action exceeds input or output bound",
        })
    }
}
impl std::error::Error for Error {}

fn text(value: &str) -> Result<(), Error> {
    if value.contains(['\0', '\r', '\n']) {
        Err(Error::InvalidParameter)
    } else {
        Ok(())
    }
}
fn atom(value: &str) -> Result<(), Error> {
    text(value)?;
    if value.is_empty() || value.starts_with(':') || value.chars().any(char::is_whitespace) {
        Err(Error::InvalidParameter)
    } else {
        Ok(())
    }
}
fn single(value: &str) -> Result<(), Error> {
    atom(value)?;
    if value.contains(',') {
        Err(Error::InvalidParameter)
    } else {
        Ok(())
    }
}

/// Validate one channel's framing without classifying its prefix.
///
/// A channel needs a prefix and a name, and cannot contain colon, comma,
/// whitespace or control characters. Callers must separately check the first
/// character against server-advertised CHANTYPES. Wire budgets are checked by
/// action builders, not by this framing-only validator.
pub fn validate_channel(channel: &str) -> Result<(), Error> {
    if channel.len() < 2
        || channel.contains([',', ':'])
        || channel.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        Err(Error::InvalidParameter)
    } else {
        Ok(())
    }
}

fn channel_list(channels: &[&str]) -> Result<String, Error> {
    // Check the list's admission/wire bound before walking caller input again.
    let result = list(channels, false)?;
    for channel in channels {
        validate_channel(channel)?;
    }
    Ok(result)
}

fn list(values: &[&str], allow_empty: bool) -> Result<String, Error> {
    if values.is_empty() && !allow_empty {
        return Err(Error::InvalidParameter);
    }
    // Bound before joining to avoid allocating arbitrarily large lists.
    if values.len() > irc::MAX_LEN_MESSAGE {
        return Err(Error::TooLong);
    }
    let mut size = values.len().saturating_sub(1);
    for value in values {
        single(value)?;
        size = size.checked_add(value.len()).ok_or(Error::TooLong)?;
        if size > irc::MAX_LEN_MESSAGE {
            return Err(Error::TooLong);
        }
    }
    Ok(values.join(","))
}
fn build(command: &str, params: Vec<String>) -> Result<Message, Error> {
    if params.len() > 15 {
        return Err(Error::TooManyParameters);
    }
    for (index, value) in params.iter().enumerate() {
        text(value)?;
        if index + 1 != params.len() {
            atom(value)?;
        }
    }
    let message = Message::new(command, params);
    let wire = message.serialize().map_err(|_| Error::InvalidParameter)?;
    irc::validate_wire_budgets(&wire).map_err(|_| Error::TooLong)?;
    Ok(message)
}
fn optional(command: &str, mut params: Vec<String>, tail: Option<&str>) -> Result<Message, Error> {
    if let Some(tail) = tail {
        params.push(tail.into());
    }
    build(command, params)
}

/// JOIN one or more channels, with optional positional channel keys.
/// Keys cannot outnumber channels; omit all keys with an empty slice.
pub fn join(channels: &[&str], keys: &[&str]) -> Result<Message, Error> {
    if keys.len() > channels.len() {
        return Err(Error::InvalidParameter);
    }
    let mut params = vec![channel_list(channels)?];
    if !keys.is_empty() {
        params.push(list(keys, false)?);
    }
    build("JOIN", params)
}
/// PART one or more channels. `Some("")` sends an explicitly empty reason.
pub fn part(channels: &[&str], reason: Option<&str>) -> Result<Message, Error> {
    optional("PART", vec![channel_list(channels)?], reason)
}
/// NAMES for specified channels, or all visible channels with an empty slice.
pub fn names(channels: &[&str]) -> Result<Message, Error> {
    query_list("NAMES", channels)
}
/// LIST channels or server-specific single-atom filters; empty means all.
pub fn list_channels(filters: &[&str]) -> Result<Message, Error> {
    query_list("LIST", filters)
}
fn query_list(command: &str, values: &[&str]) -> Result<Message, Error> {
    let params = if values.is_empty() {
        Vec::new()
    } else {
        vec![list(values, false)?]
    };
    build(command, params)
}
/// Request a new nickname. Server nickname grammar/limits remain server policy.
pub fn nick(nickname: &str) -> Result<Message, Error> {
    single(nickname)?;
    build("NICK", vec![nickname.into()])
}
/// Mark away; `None` or an empty reason clears away status.
pub fn away(reason: Option<&str>) -> Result<Message, Error> {
    optional("AWAY", Vec::new(), reason.filter(|s| !s.is_empty()))
}
/// Request WHOIS for one nickname.
pub fn whois(nickname: &str) -> Result<Message, Error> {
    single(nickname)?;
    build("WHOIS", vec![nickname.into()])
}
/// Send QUIT with an optional reason; this does not close any transport.
pub fn quit(reason: Option<&str>) -> Result<Message, Error> {
    optional("QUIT", Vec::new(), reason)
}
/// Query MODE with `None`, or change/query individual modes with `Some`.
/// Mode grammar and argument arity depend on ISUPPORT and are caller policy.
pub fn mode(target: &str, modes: Option<&str>, args: &[&str]) -> Result<Message, Error> {
    single(target)?;
    if args.len() > 13 {
        return Err(Error::TooManyParameters);
    }
    let mut params = vec![target.into()];
    if let Some(modes) = modes {
        atom(modes)?;
        params.push(modes.into());
        for arg in args {
            atom(arg)?;
            params.push((*arg).into());
        }
    } else if !args.is_empty() {
        return Err(Error::InvalidParameter);
    }
    build("MODE", params)
}
/// Add or remove a channel ban mask. Listing bans uses `mode(channel, Some("+b"), &[])`.
pub fn ban(channel: &str, mask: &str, add: bool) -> Result<Message, Error> {
    mode(channel, Some(if add { "+b" } else { "-b" }), &[mask])
}
/// KICK one nickname from a channel, with an optional reason.
pub fn kick(channel: &str, nickname: &str, reason: Option<&str>) -> Result<Message, Error> {
    single(channel)?;
    single(nickname)?;
    optional("KICK", vec![channel.into(), nickname.into()], reason)
}
/// Invite one nickname to one channel.
pub fn invite(nickname: &str, channel: &str) -> Result<Message, Error> {
    single(nickname)?;
    single(channel)?;
    build("INVITE", vec![nickname.into(), channel.into()])
}
/// Query a topic with `None`, set it with `Some(text)`, clear with `Some("")`.
pub fn topic(channel: &str, value: Option<&str>) -> Result<Message, Error> {
    single(channel)?;
    optional("TOPIC", vec![channel.into()], value)
}
/// Typed MONITOR subcommand. Capability/ISUPPORT checks remain caller policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Monitor {
    /// Add nicknames.
    Add,
    /// Remove nicknames.
    Remove,
    /// Clear the watch list.
    Clear,
    /// Request the watch list.
    List,
    /// Request online/offline status of watched nicknames.
    Status,
}
/// Build MONITOR. Add/remove require names; other subcommands require no names.
pub fn monitor(action: Monitor, nicknames: &[&str]) -> Result<Message, Error> {
    let code = match action {
        Monitor::Add => "+",
        Monitor::Remove => "-",
        Monitor::Clear => "C",
        Monitor::List => "L",
        Monitor::Status => "S",
    };
    let mut params = vec![code.into()];
    if matches!(action, Monitor::Add | Monitor::Remove) {
        params.push(list(nicknames, false)?);
    } else if !nicknames.is_empty() {
        return Err(Error::InvalidParameter);
    }
    build("MONITOR", params)
}

/// Split a PRIVMSG, preserving every text byte (including whitespace).
///
/// `relay_prefix_bytes` reserves the complete server-added `:nick!user@host `
/// prefix, including delimiters. Use a conservative allowance until the actual
/// mask is known. A value of zero protects only the outgoing, unprefixed line.
/// Word boundaries are preferred, but UTF-8 characters are never cut. Errors
/// return no partial result. Tags are not included in the non-tag IRC budget.
pub fn privmsg(
    target: &str,
    value: &str,
    relay_prefix_bytes: usize,
) -> Result<Vec<Message>, Error> {
    split("PRIVMSG", target, value, relay_prefix_bytes, false)
}
/// Split NOTICE with the same lossless UTF-8 and relay-budget rules as `privmsg`.
pub fn notice(target: &str, value: &str, relay_prefix_bytes: usize) -> Result<Vec<Message>, Error> {
    split("NOTICE", target, value, relay_prefix_bytes, false)
}
/// Split CTCP ACTION, wrapping every chunk separately. Embedded CTCP delimiters
/// are rejected so the caller cannot accidentally produce nested CTCP frames.
pub fn action(target: &str, value: &str, relay_prefix_bytes: usize) -> Result<Vec<Message>, Error> {
    if value.contains('\x01') {
        return Err(Error::InvalidParameter);
    }
    split("PRIVMSG", target, value, relay_prefix_bytes, true)
}
fn split(
    command: &str,
    target: &str,
    value: &str,
    prefix: usize,
    action: bool,
) -> Result<Vec<Message>, Error> {
    single(target)?;
    text(value)?;
    if value.len() > MAX_TEXT_BYTES {
        return Err(Error::TooLarge);
    }
    // Always reserve a trailing ':' even if serialization can omit it.
    // command + SP + target + SP + ':' + text + CRLF.
    let framing = if action { "\x01ACTION ".len() + 1 } else { 0 };
    let budget = irc::MAX_LEN_MESSAGE
        .checked_sub(prefix)
        .and_then(|n| n.checked_sub(command.len()))
        .and_then(|n| n.checked_sub(target.len()))
        .and_then(|n| n.checked_sub(5 + framing))
        .ok_or(Error::InsufficientBudget)?;
    if budget == 0 || value.chars().any(|c| c.len_utf8() > budget) {
        return Err(Error::InsufficientBudget);
    }
    let mut messages = Vec::new();
    let mut remaining = value;
    loop {
        if messages.len() == MAX_PARTS {
            return Err(Error::TooLarge);
        }
        let mut cut = remaining.len().min(budget);
        while !remaining.is_char_boundary(cut) {
            cut -= 1;
        }
        if cut < remaining.len() {
            // Keep the separating space rather than silently dropping user text.
            if let Some(space) = remaining[..cut].rfind(' ') {
                cut = space + 1;
            }
        }
        let (chunk, rest) = remaining.split_at(cut);
        let payload = if action {
            format!("\x01ACTION {chunk}\x01")
        } else {
            chunk.into()
        };
        messages.push(build(command, vec![target.into(), payload])?);
        remaining = rest;
        if remaining.is_empty() {
            break;
        }
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wire(message: Result<Message, Error>) -> String {
        message.unwrap().serialize().unwrap()
    }
    #[test]
    fn action_builders() {
        let cases = [
            (join(&["#a", "#b"], &["key"]), "JOIN #a,#b key\r\n"),
            (part(&["#a"], Some("see you")), "PART #a :see you\r\n"),
            (names(&[]), "NAMES\r\n"),
            (names(&["#a", "#b"]), "NAMES #a,#b\r\n"),
            (list_channels(&[">50"]), "LIST >50\r\n"),
            (nick("new"), "NICK new\r\n"),
            (away(Some("out now")), "AWAY :out now\r\n"),
            (away(Some("")), "AWAY\r\n"),
            (whois("alice"), "WHOIS alice\r\n"),
            (quit(None), "QUIT\r\n"),
            (mode("#a", None, &[]), "MODE #a\r\n"),
            (
                mode("#a", Some("+ov"), &["alice", "bob"]),
                "MODE #a +ov alice bob\r\n",
            ),
            (ban("#a", "*!*@host", true), "MODE #a +b *!*@host\r\n"),
            (ban("#a", "*!*@host", false), "MODE #a -b *!*@host\r\n"),
            (kick("#a", "alice", Some("bye")), "KICK #a alice bye\r\n"),
            (invite("alice", "#a"), "INVITE alice #a\r\n"),
            (topic("#a", None), "TOPIC #a\r\n"),
            (topic("#a", Some("")), "TOPIC #a :\r\n"),
            (monitor(Monitor::Add, &["a", "b"]), "MONITOR + a,b\r\n"),
            (monitor(Monitor::Remove, &["a"]), "MONITOR - a\r\n"),
            (monitor(Monitor::Clear, &[]), "MONITOR C\r\n"),
            (monitor(Monitor::List, &[]), "MONITOR L\r\n"),
            (monitor(Monitor::Status, &[]), "MONITOR S\r\n"),
        ];
        for (result, expected) in cases {
            assert_eq!(wire(result), expected);
        }
    }
    #[test]
    fn channel_framing_is_shared_by_join_and_part_without_fixed_prefixes() {
        for channel in ["#normal", "&local", "+modeless", "!safe", "~custom"] {
            validate_channel(channel).unwrap();
            assert_eq!(wire(join(&[channel], &[])), format!("JOIN {channel}\r\n"));
            assert_eq!(wire(part(&[channel], None)), format!("PART {channel}\r\n"));
        }
        for channel in [
            "",
            "#",
            "#a:b",
            "~a:b",
            "#a,b",
            "#a b",
            "#a\tb",
            "#a\r\nQUIT",
            "#a\0b",
            "#a\u{7}b",
        ] {
            assert_eq!(validate_channel(channel), Err(Error::InvalidParameter));
            assert_eq!(
                join(&["#valid", channel], &[]),
                Err(Error::InvalidParameter)
            );
            assert_eq!(
                part(&["#valid", channel], None),
                Err(Error::InvalidParameter)
            );
        }
    }

    #[test]
    fn reject_injection_invalid_arguments_and_oversize() {
        for invalid in ["", ":nick", "a b", "a\tb", "a,b", "a\r\nQUIT", "a\0b"] {
            assert!(nick(invalid).is_err());
            assert!(join(&[invalid], &[]).is_err());
            assert!(privmsg(invalid, "hello", 0).is_err());
        }
        for invalid in ["x\r", "x\n", "x\0"] {
            assert!(topic("#a", Some(invalid)).is_err());
            assert!(privmsg("#a", invalid, 0).is_err());
        }
        assert!(join(&[], &[]).is_err());
        assert!(join(&["#a"], &["a", "b"]).is_err());
        assert!(mode("#a", None, &["a"]).is_err());
        assert_eq!(
            mode("#a", Some("+o"), &["a"; 14]),
            Err(Error::TooManyParameters)
        );
        assert!(monitor(Monitor::Add, &[]).is_err());
        assert!(monitor(Monitor::Clear, &["a"]).is_err());
        assert!(action("#a", "bad\x01PING", 0).is_err());
        assert_eq!(topic("#a", Some(&"s".repeat(512))), Err(Error::TooLong));
    }
    #[test]
    fn splitting_is_lossless_and_fits_relay() {
        let prefix = "nick!user@host";
        let reserved = prefix.len() + 2;
        for value in [
            "".into(),
            "x".repeat(2000),
            "世🙂  hello   ".repeat(150),
            " ".repeat(2000),
        ] {
            for (builder, command, is_action) in [
                (
                    privmsg as fn(&str, &str, usize) -> Result<Vec<Message>, Error>,
                    "PRIVMSG",
                    false,
                ),
                (notice, "NOTICE", false),
                (action, "PRIVMSG", true),
            ] {
                let messages = builder("#target", &value, reserved).unwrap();
                let mut rebuilt = String::new();
                for mut message in messages {
                    assert_eq!(message.command, command);
                    let chunk = &message.params[1];
                    rebuilt.push_str(if is_action {
                        chunk
                            .strip_prefix("\x01ACTION ")
                            .unwrap()
                            .strip_suffix('\x01')
                            .unwrap()
                    } else {
                        chunk
                    });
                    message.source = Some(prefix.into());
                    let wire = message.serialize().unwrap();
                    irc::validate_wire_budgets(&wire).unwrap();
                    assert!(wire.len() <= 512);
                }
                assert_eq!(rebuilt, value);
            }
        }
    }
    #[test]
    fn exact_boundaries_and_impossible_budgets() {
        let budget = 512 - "PRIVMSG".len() - "#a".len() - 5;
        assert_eq!(privmsg("#a", &"a".repeat(budget), 0).unwrap().len(), 1);
        assert_eq!(privmsg("#a", &"a".repeat(budget + 1), 0).unwrap().len(), 2);
        assert_eq!(
            privmsg("#a", "🙂", budget - 3),
            Err(Error::InsufficientBudget)
        );
        assert_eq!(
            privmsg("#a", "x", usize::MAX),
            Err(Error::InsufficientBudget)
        );
        assert_eq!(
            privmsg("#a", &"x".repeat(MAX_TEXT_BYTES + 1), 0),
            Err(Error::TooLarge)
        );
        assert_eq!(
            privmsg("#a", &"x".repeat(257), budget - 1),
            Err(Error::TooLarge)
        );
    }
}
