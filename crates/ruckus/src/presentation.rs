//! Lossless presentation of the model's already-sanitized history lines.
//!
//! These formats are display conventions, not authenticated message metadata.
//! Only exact envelopes are split; everything else remains a whole system row.
//! This module does not sanitize, trim, interpret markup, or infer timestamps.

/// The display treatment for a history line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RowKind {
    Chat,
    /// Locally queued, not evidence of delivery.
    Queued,
    /// Written/flushed to the connection, not a recipient delivery receipt.
    Sent,
    /// Connection ended or confirmation events were lost.
    Unconfirmed,
    System,
}

/// A display row with the original author and body spelling preserved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MessageRow {
    pub kind: RowKind,
    pub author: Option<String>,
    pub body: String,
}

/// Split only the model's `<nick> body` and outgoing status envelopes.
///
/// Empty bodies are valid. Whitespace and delimiters inside the body remain
/// untouched, including strings that themselves resemble messages or markup.
/// Malformed or ambiguous author tokens fall back to a whole system row rather
/// than hiding any of their content. This is deliberately not IRC nick validation:
/// server-specific Unicode nicknames are supported without a protocol dependency.
pub(crate) fn message_row(line: &str) -> MessageRow {
    let incoming = line
        .strip_prefix('<')
        .and_then(|rest| rest.split_once("> "))
        .map(|(author, body)| (RowKind::Chat, author, body));
    let outgoing = || {
        [
            (" (queued): ", RowKind::Queued),
            (" (sent): ", RowKind::Sent),
            (" (unconfirmed): ", RowKind::Unconfirmed),
        ]
        .into_iter()
        .find_map(|(envelope, kind)| {
            line.split_once(envelope)
                .filter(|(author, _)| author_token(author))
                .map(|(author, body)| (kind, author, body))
        })
    };
    if let Some((kind, author, body)) = incoming.or_else(outgoing)
        && author_token(author)
    {
        return MessageRow {
            kind,
            author: Some(author.into()),
            body: body.into(),
        };
    }
    MessageRow {
        kind: RowKind::System,
        author: None,
        body: line.into(),
    }
}

fn author_token(author: &str) -> bool {
    !author.is_empty()
        && !author
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, '<' | '>'))
}

/// Whether adjacent rows can share a chat author heading.
///
/// Exact spelling is intentional: no negotiated IRC casemapping is available
/// here. Outgoing-status and system rows always interrupt a group.
pub(crate) fn same_author(previous: &MessageRow, current: &MessageRow) -> bool {
    previous.kind == RowKind::Chat
        && current.kind == RowKind::Chat
        && previous.author.as_deref().is_some_and(author_token)
        && previous.author == current.author
}

/// One Unicode scalar for a decorative initial, or `?` for an empty/unsafe one.
///
/// This is not a grapheme or an avatar. Uppercase expansion is limited to one
/// scalar, so even unusual Unicode input cannot create a large label.
pub(crate) fn author_initial(author: &str) -> String {
    if author
        .chars()
        .next()
        .is_some_and(|ch| ch.is_whitespace() || ch.is_control())
    {
        return "?".into();
    }
    let author = ruckus_core::sanitize::sanitize_terminal(author);
    match author.chars().next() {
        Some(ch) if !ch.is_whitespace() && !ch.is_control() => ch.to_uppercase().take(1).collect(),
        _ => "?".into(),
    }
}

/// A stable decorative RGB accent, not a presence or identity indicator.
pub(crate) fn author_color(author: &str) -> u32 {
    // Restrained iris, sage, dusty blue, rose and sand on warm charcoal.
    const PALETTE: [u32; 5] = [0xa99aff, 0x9fbaa8, 0x9daecb, 0xc5a1af, 0xc6b394];
    // Explicit wrapping arithmetic keeps this deterministic across builds and
    // platforms, unlike the standard library's randomized map hasher.
    let hash = author.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    PALETTE[(hash % PALETTE.len() as u32) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outgoing_statuses_preserve_exec_and_do_not_parse_status_words_inside_the_body() {
        for (label, kind) in [
            ("queued", RowKind::Queued),
            ("sent", RowKind::Sent),
            ("unconfirmed", RowKind::Unconfirmed),
        ] {
            let row = message_row(&format!("exec ({label}): body (queued): still body"));
            assert_eq!(row.author.as_deref(), Some("exec"));
            assert_eq!(row.kind, kind);
            assert_eq!(row.body, "body (queued): still body");
        }
    }

    #[test]
    fn decorative_initials_cannot_inject_bidi_controls() {
        assert_eq!(author_initial("\u{202e}exec"), "E");
        assert_eq!(author_initial("\u{2066}\u{2069}nick"), "N");
        assert_eq!(author_initial("\u{202e}"), "?");
    }

    #[test]
    fn exact_envelopes_preserve_authors_and_bodies() {
        for (line, kind, author, body) in [
            ("<nick> hello", RowKind::Chat, "nick", "hello"),
            ("<nick> ", RowKind::Chat, "nick", ""),
            ("<猫🦀>  café  ", RowKind::Chat, "猫🦀", " café  "),
            ("me (queued): hello", RowKind::Queued, "me", "hello"),
            ("me (queued): ", RowKind::Queued, "me", ""),
            (
                "<nick> <other> hi (queued): <script>& text",
                RowKind::Chat,
                "nick",
                "<other> hi (queued): <script>& text",
            ),
            (
                "Éloi (queued): \t\x1b[31m\n\u{202e}text\0",
                RowKind::Queued,
                "Éloi",
                "\t\x1b[31m\n\u{202e}text\0",
            ),
        ] {
            let row = message_row(line);
            assert_eq!(row.kind, kind);
            assert_eq!(row.author.as_deref(), Some(author));
            assert_eq!(row.body, body);
        }
    }

    #[test]
    fn unknown_system_and_malformed_lines_remain_whole() {
        for line in [
            "",
            "→ nick joined",
            "← nick left",
            "← nick quit: <other> hello",
            "nick is now known as other",
            "Disconnected: reason",
            "NOTICE <nick> text",
            "<> text",
            "<nick>text",
            "<nick>",
            "<nick",
            "<nick name> text",
            "<<nick> text",
            "<ni>ck> text",
            "<ni\tck> text",
            " (queued): text",
            "nick (queued):text",
            "nick (queued):",
            "nick name (queued): text",
            "nick\nname (queued): text",
            "\x1b[31m\0\u{202e}hostile <nick> text",
            "<script>alert('x')</script>",
            "  leading and trailing  ",
        ] {
            assert_eq!(
                message_row(line),
                MessageRow {
                    kind: RowKind::System,
                    author: None,
                    body: line.into(),
                },
                "{line:?}"
            );
        }
    }

    #[test]
    fn arbitrary_bodies_round_trip_without_recursive_interpretation() {
        let bodies = [
            "",
            "  ",
            "> ",
            "<nick> hello",
            "me (queued): hello",
            "→ nick joined",
            "a\0b\nc\r\t\x1b[31m",
            "🦀e\u{301}\u{200d}\u{202e}漢字",
        ];
        for body in bodies {
            for line in [
                format!("<猫> {body}"),
                format!("猫 (queued): {body}"),
                format!("猫 (sent): {body}"),
                format!("猫 (unconfirmed): {body}"),
                format!("unknown: {body}"),
            ] {
                let row = message_row(&line);
                let rebuilt = match row.kind {
                    RowKind::Chat => format!("<{}> {}", row.author.unwrap(), row.body),
                    RowKind::Queued => format!("{} (queued): {}", row.author.unwrap(), row.body),
                    RowKind::Sent => format!("{} (sent): {}", row.author.unwrap(), row.body),
                    RowKind::Unconfirmed => {
                        format!("{} (unconfirmed): {}", row.author.unwrap(), row.body)
                    }
                    RowKind::System => row.body,
                };
                assert_eq!(rebuilt, line);
            }
        }
        let body = "🦀".repeat(20_000);
        assert_eq!(message_row(&format!("<nick> {body}")).body, body);
    }

    #[test]
    fn only_consecutive_actual_chat_authors_group() {
        let chat = message_row("<Nick> first");
        assert!(same_author(&chat, &message_row("<Nick> second")));
        for other in [
            message_row("<nick> case differs"),
            message_row("<Other> text"),
            message_row("Nick (queued): pending"),
            message_row("→ Nick joined"),
            MessageRow {
                kind: RowKind::Chat,
                author: None,
                body: "missing author".into(),
            },
        ] {
            assert!(!same_author(&chat, &other));
            assert!(!same_author(&other, &chat));
            if other.kind != RowKind::Chat || other.author.is_none() {
                assert!(!same_author(&other, &other));
            }
        }
    }

    #[test]
    fn initials_are_unicode_safe_and_bounded() {
        for (author, initial) in [
            ("alice", "A"),
            ("élise", "É"),
            ("猫", "猫"),
            ("🦀coder", "🦀"),
            ("ßeta", "S"),
            ("e\u{301}loi", "E"),
            ("", "?"),
            (" nick", "?"),
            ("\0nick", "?"),
        ] {
            let actual = author_initial(author);
            assert_eq!(actual, initial);
            assert_eq!(actual.chars().count(), 1);
        }
        assert_eq!(author_initial(&"猫".repeat(20_000)), "猫");
    }

    #[test]
    fn colors_are_stable_and_restrained() {
        assert_eq!(author_color("alice"), 0xc6b394);
        for author in ["", "alice", "猫", "🦀", "\0\nhostile"] {
            let color = author_color(author);
            assert_eq!(color, author_color(author));
            assert!([0xa99aff, 0x9fbaa8, 0x9daecb, 0xc5a1af, 0xc6b394].contains(&color));
        }
    }
}
