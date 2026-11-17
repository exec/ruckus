//! Terminal-bound sanitization, adapted from Lurk's `client/sanitize.go`.
//!
//! Copyright (c) 2026 exec. Licensed under the MIT License.
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

use std::borrow::Cow;

/// Neutralize terminal controls in untrusted text before adding renderer styles.
///
/// C0/C1 controls (including ESC, DEL, IRC formatting, CR and LF) and explicit
/// Unicode bidi controls are removed. Tabs and Unicode line/paragraph separators
/// become spaces. **No newlines survive**: render multiline messages by splitting
/// into logical lines first and sanitizing each line separately. Printable
/// Unicode, including natural RTL text and emoji joiners, is preserved.
///
/// Escape sequence payloads may remain visible, but cannot execute without their
/// control introducers. Parse CTCP framing before calling this function.
pub fn sanitize_terminal(text: &str) -> Cow<'_, str> {
    if !text.chars().any(unsafe_control) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .filter_map(|c| {
                if matches!(c, '\t' | '\u{2028}' | '\u{2029}') {
                    Some(' ')
                } else if unsafe_control(c) {
                    None
                } else {
                    Some(c)
                }
            })
            .collect(),
    )
}

fn unsafe_control(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}'
                | '\u{200e}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_terminal_and_layout_controls() {
        assert_eq!(
            sanitize_terminal("\x1b]52;c;secret\x07a\u{009b}2J\tb\r\nc\u{2028}d\u{2029}e"),
            "]52;c;secreta2J bc d e"
        );
        for c in (0..=0x9f).filter_map(char::from_u32) {
            if c.is_control() {
                let input = format!("a{c}b");
                assert_eq!(
                    sanitize_terminal(&input),
                    if c == '\t' { "a b" } else { "ab" }
                );
            }
        }
    }

    #[test]
    fn removes_bidi_but_preserves_printable_unicode() {
        for c in [
            '\u{061c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}',
            '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            assert_eq!(sanitize_terminal(&format!("a{c}b")), "ab");
        }
        assert!(matches!(
            sanitize_terminal("שלום العربية 👩‍💻 café"),
            Cow::Borrowed(_)
        ));
    }
}
