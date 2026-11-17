//! Pure CTCP helpers; automatic replies and rate limiting belong to the client.
//!
//! Inspired by Lurk's `client/ctcp.go` (Copyright (c) 2026 exec, MIT).

/// A borrowed CTCP payload. Contents remain untrusted and need sanitization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ctcp<'a> {
    /// CTCP command, preserving wire case.
    pub command: &'a str,
    /// Payload after the first space, or empty when absent.
    pub argument: &'a str,
}

/// Parse exactly one fully framed CTCP body.
///
/// Rejects missing delimiters, embedded delimiters and CR/LF/NUL. This is stricter
/// than Lurk's lenient ACTION helper: malformed framing is ordinary message text,
/// not an automatic-reply request. No unescaping or network actions are performed.
pub fn parse_ctcp(body: &str) -> Option<Ctcp<'_>> {
    let inner = body.strip_prefix('\x01')?.strip_suffix('\x01')?;
    if inner
        .chars()
        .any(|c| matches!(c, '\x01' | '\r' | '\n' | '\0'))
    {
        return None;
    }
    let (command, argument) = inner.split_once(' ').unwrap_or((inner, ""));
    if command.is_empty() || !command.bytes().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(Ctcp { command, argument })
}

/// Extract ACTION text (including an empty action), case-insensitively.
///
/// The returned text must still pass through terminal sanitization.
pub fn ctcp_action(body: &str) -> Option<&str> {
    let ctcp = parse_ctcp(body)?;
    ctcp.command
        .eq_ignore_ascii_case("ACTION")
        .then_some(ctcp.argument)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_actions_and_queries_without_replying() {
        assert_eq!(ctcp_action("\x01ACTION waves\x01"), Some("waves"));
        assert_eq!(ctcp_action("\x01action\x01"), Some(""));
        assert_eq!(parse_ctcp("\x01PING 123\x01").unwrap().argument, "123");
        assert_eq!(ctcp_action("\x01VERSION\x01"), None);
    }

    #[test]
    fn rejects_ambiguous_or_injected_framing() {
        for body in [
            "ACTION waves",
            "\x01ACTION waves",
            "\x01\x01",
            "\x01PING a\r\nb\x01",
            "\x01PING a\x01\x01VERSION\x01",
            "\x01PING \0\x01",
        ] {
            assert_eq!(parse_ctcp(body), None);
        }
    }
}
