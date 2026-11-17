//! Pure IRCv3 batch classification and CHATHISTORY request construction.
//!
//! Based on Lurk's `client/{actions,event,run,state}.go`.
//!
//! These helpers do not negotiate capabilities, send requests, collect history,
//! mutate channel state, or integrate a frontend. Callers must negotiate `batch`
//! and `draft/chathistory`, feed messages in wire order, capture classification
//! before asynchronous delivery, and clear tracking state on disconnect.
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

use crate::irc::{self, Message};
use std::collections::BTreeMap;
use std::fmt;

/// Maximum simultaneously tracked batches, matching Lurk's defensive cap.
pub const MAX_OPEN_BATCHES: usize = 256;

/// Invalid history request or batch state transition.
///
/// Error messages intentionally omit remote text and request contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Error {
    /// A BATCH command has missing, invalid, or excess fields.
    MalformedBatch,
    /// A nested opening names an untracked parent.
    UnknownParent,
    /// An opening reuses an already open reference.
    DuplicateBatch,
    /// The open-batch map is full.
    TooManyBatches,
    /// A request has an empty or invalid protocol token.
    InvalidRequest,
    /// IRC serialization or wire-budget validation failed.
    Wire(irc::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedBatch => f.write_str("history: malformed batch"),
            Self::UnknownParent => f.write_str("history: unknown batch parent"),
            Self::DuplicateBatch => f.write_str("history: duplicate open batch"),
            Self::TooManyBatches => f.write_str("history: too many open batches"),
            Self::InvalidRequest => f.write_str("history: invalid request token"),
            Self::Wire(error) => write!(f, "history: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Wire(error) => Some(error),
            _ => None,
        }
    }
}

impl From<irc::Error> for Error {
    fn from(error: irc::Error) -> Self {
        Self::Wire(error)
    }
}

/// Owned event metadata captured while its enclosing batch is still open.
///
/// An unknown or missing `batch` tag produces the default (unclassified)
/// value, not proof that an event is live. Server-time alone does not imply
/// replay. Opening/closing control messages describe their enclosing `@batch`,
/// not the batch reference in their first parameter.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Classification {
    /// The directly enclosing batch's reference, without `+` or `-`.
    pub batch_reference: Option<String>,
    /// The directly enclosing batch's type, preserving wire spelling.
    pub batch_type: Option<String>,
    /// Whether the batch or any ancestor is a known history/playback batch.
    pub is_replay: bool,
}

#[derive(Clone, Debug)]
struct Batch {
    parent: Option<String>,
    kind: String,
    replay: bool,
}

/// Bounded batch state for one connection; stores metadata, never message bodies.
///
/// Unlike Lurk's flat map, nested batches inherit replay classification.
/// Duplicate openings and unknown parents are rejected instead of replacing
/// active state. Closing a parent also drops its descendants to avoid stale
/// references if the server closes batches out of order.
#[derive(Debug, Default)]
pub struct BatchTracker {
    batches: BTreeMap<String, Batch>,
}

impl BatchTracker {
    /// Creates an empty tracker with a fixed [`MAX_OPEN_BATCHES`] cap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of currently open tracked batches.
    pub fn len(&self) -> usize {
        self.batches.len()
    }

    /// Reports whether no batches are tracked.
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// Drops connection-scoped metadata, for example on disconnect/reconnect.
    pub fn clear(&mut self) {
        self.batches.clear();
    }

    /// Classifies an event using its `batch` tag without changing tracking state.
    pub fn classify(&self, message: &Message) -> Classification {
        let Some(reference) = message.tags.get("batch") else {
            return Classification::default();
        };
        let Some(batch) = self.batches.get(reference) else {
            return Classification::default();
        };
        Classification {
            batch_reference: Some(reference.clone()),
            batch_type: Some(batch.kind.clone()),
            is_replay: batch.replay,
        }
    }

    /// Classifies a message and applies any BATCH opening/closing transition.
    ///
    /// Call exactly once per received message in wire order. Non-BATCH messages
    /// are only classified. For BATCH messages, wire validation and the 512-byte
    /// body budget bound retained metadata even for manually constructed input.
    /// References/types must be nonempty protocol tokens. Unknown closes are
    /// harmless; duplicate opens, unknown parents, malformed commands and
    /// capacity failures return errors without changing state.
    pub fn observe(&mut self, message: &Message) -> Result<Classification, Error> {
        let classification = self.classify(message);
        if !message.command.eq_ignore_ascii_case("BATCH") {
            return Ok(classification);
        }
        checked(message)?;
        let reference = message.param(0);
        let (opening, reference) = if let Some(reference) = reference.strip_prefix('+') {
            (true, reference)
        } else if let Some(reference) = reference.strip_prefix('-') {
            (false, reference)
        } else {
            return Err(Error::MalformedBatch);
        };
        if !token(reference) {
            return Err(Error::MalformedBatch);
        }
        if !opening {
            if message.params.len() != 1 {
                return Err(Error::MalformedBatch);
            }
            self.close(reference);
            return Ok(classification);
        }
        let kind = message.param(1);
        if !token(kind) {
            return Err(Error::MalformedBatch);
        }
        if self.batches.contains_key(reference) {
            return Err(Error::DuplicateBatch);
        }
        let parent = message.tags.get("batch");
        if parent.is_some_and(|reference| !self.batches.contains_key(reference)) {
            return Err(Error::UnknownParent);
        }
        if self.batches.len() >= MAX_OPEN_BATCHES {
            return Err(Error::TooManyBatches);
        }
        self.batches.insert(
            reference.into(),
            Batch {
                parent: parent.cloned(),
                kind: kind.into(),
                replay: classification.is_replay || replay_type(kind),
            },
        );
        Ok(classification)
    }

    fn close(&mut self, reference: &str) {
        if self.batches.remove(reference).is_none() {
            return;
        }
        // Depth is bounded by map capacity. Iteration avoids stack recursion;
        // every pass removes at least one orphan or terminates.
        loop {
            let orphan = self.batches.iter().find_map(|(reference, batch)| {
                batch
                    .parent
                    .as_ref()
                    .filter(|parent| !self.batches.contains_key(*parent))
                    .map(|_| reference.clone())
            });
            let Some(orphan) = orphan else {
                break;
            };
            self.batches.remove(&orphan);
        }
    }
}

fn replay_type(kind: &str) -> bool {
    matches!(
        kind,
        "chathistory" | "draft/chathistory-targets" | "znc.in/playback"
    )
}

/// CHATHISTORY request builder with an optional advertised server limit.
///
/// Construction alone does not check negotiated capabilities. Returned messages
/// are validated and fit the non-tag IRC wire budget. No request is sent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HistoryRequests {
    server_limit: Option<usize>,
}

impl HistoryRequests {
    /// Creates a builder from an optional `CHATHISTORY` ISUPPORT value.
    ///
    /// Positive decimal integers cap requests; missing, zero, invalid and
    /// overflowing values are ignored, matching Lurk's behavior.
    pub fn new(advertised_limit: Option<&str>) -> Self {
        Self {
            server_limit: advertised_limit
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| *value > 0),
        }
    }

    /// Clamps nonpositive requests to one, then applies the advertised maximum.
    pub fn limit(&self, requested: i64) -> usize {
        let requested = usize::try_from(requested.max(1)).unwrap_or(usize::MAX);
        self.server_limit
            .map_or(requested, |limit| requested.min(limit))
    }

    /// Builds `CHATHISTORY LATEST <target> * <limit>`.
    pub fn latest(&self, target: &str, limit: i64) -> Result<Message, Error> {
        self.build("LATEST", target, "*", limit)
    }

    /// Builds `CHATHISTORY BEFORE <target> <reference> <limit>`.
    ///
    /// A reference is `msgid=<id>` or `timestamp=<timestamp>`. Timestamp contents
    /// are treated as opaque protocol tokens, not parsed as calendar dates.
    pub fn before(&self, target: &str, reference: &str, limit: i64) -> Result<Message, Error> {
        reference_token(reference)?;
        self.build("BEFORE", target, reference, limit)
    }

    /// Builds `CHATHISTORY AFTER <target> <reference> <limit>`.
    ///
    /// Reference validation is identical to [`Self::before`].
    pub fn after(&self, target: &str, reference: &str, limit: i64) -> Result<Message, Error> {
        reference_token(reference)?;
        self.build("AFTER", target, reference, limit)
    }

    /// Builds `CHATHISTORY TARGETS timestamp=<from> timestamp=<to> <limit>`.
    ///
    /// Pass unprefixed RFC3339 UTC timestamp strings. No calendar parsing or
    /// timezone conversion is performed; only token/framing validation occurs.
    /// A descending range is permitted for newest-first pagination.
    pub fn targets(&self, from: &str, to: &str, limit: i64) -> Result<Message, Error> {
        if !token(from) || !token(to) {
            return Err(Error::InvalidRequest);
        }
        self.build(
            "TARGETS",
            &format!("timestamp={from}"),
            &format!("timestamp={to}"),
            limit,
        )
    }

    fn build(
        &self,
        subcommand: &str,
        first: &str,
        second: &str,
        limit: i64,
    ) -> Result<Message, Error> {
        if !token(first) || !token(second) {
            return Err(Error::InvalidRequest);
        }
        let message = Message::new(
            "CHATHISTORY",
            vec![
                subcommand.into(),
                first.into(),
                second.into(),
                self.limit(limit).to_string(),
            ],
        );
        checked(&message)?;
        Ok(message)
    }
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(':')
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
}

fn reference_token(value: &str) -> Result<(), Error> {
    let data = value
        .strip_prefix("msgid=")
        .or_else(|| value.strip_prefix("timestamp="));
    if !token(value) || data.is_none_or(|data| !token(data)) {
        Err(Error::InvalidRequest)
    } else {
        Ok(())
    }
}

fn checked(message: &Message) -> Result<(), Error> {
    irc::validate_wire_budgets(&message.serialize()?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Message {
        Message::parse(line).unwrap()
    }

    #[test]
    fn nested_replay_is_snapshotted_before_close() {
        let mut tracker = BatchTracker::new();
        tracker
            .observe(&parse("BATCH +h chathistory #chan"))
            .unwrap();
        let opening = tracker
            .observe(&parse("@batch=h BATCH +m draft/multiline #chan"))
            .unwrap();
        assert!(opening.is_replay);
        let nested = tracker
            .observe(&parse("@batch=m :n PRIVMSG #chan :old"))
            .unwrap();
        assert_eq!(nested.batch_type.as_deref(), Some("draft/multiline"));
        assert_eq!(nested.batch_reference.as_deref(), Some("m"));
        assert!(nested.is_replay);
        tracker.observe(&parse("BATCH -h")).unwrap();
        assert!(tracker.is_empty());
        assert!(nested.is_replay);
        assert_eq!(
            tracker.classify(&parse("@batch=m PRIVMSG #chan :unknown")),
            Classification::default()
        );
    }

    #[test]
    fn known_history_types_only_and_siblings() {
        let mut tracker = BatchTracker::new();
        for kind in [
            "chathistory",
            "draft/chathistory-targets",
            "znc.in/playback",
            "netjoin",
            "netsplit",
            "unknown",
        ] {
            tracker
                .observe(&parse(&format!("BATCH +a {kind}")))
                .unwrap();
            let classification = tracker.observe(&parse("@batch=a CMD")).unwrap();
            assert_eq!(classification.is_replay, replay_type(kind));
            tracker.observe(&parse("BATCH -a")).unwrap();
        }
        tracker.observe(&parse("BATCH +a netjoin")).unwrap();
        tracker
            .observe(&parse("BATCH +b chathistory #chan"))
            .unwrap();
        assert!(!tracker.classify(&parse("@batch=a JOIN #chan")).is_replay);
        assert!(
            tracker
                .classify(&parse("@batch=b PRIVMSG #chan :old"))
                .is_replay
        );
        assert_eq!(
            tracker.classify(&parse("@time=2020-01-01T00:00:00Z CMD")),
            Classification::default()
        );
        tracker.clear();
        assert!(tracker.is_empty());
    }

    #[test]
    fn malformed_transitions_do_not_mutate_state() {
        let mut tracker = BatchTracker::new();
        tracker.observe(&parse("BATCH +a chathistory")).unwrap();
        for line in [
            "BATCH",
            "BATCH +",
            "BATCH -",
            "BATCH a",
            "BATCH +b",
            "BATCH -a extra",
            "BATCH +b :bad type",
        ] {
            assert_eq!(
                tracker.observe(&parse(line)),
                Err(Error::MalformedBatch),
                "{line}"
            );
            assert_eq!(tracker.len(), 1);
        }
        assert_eq!(
            tracker.observe(&parse("BATCH +a netjoin")),
            Err(Error::DuplicateBatch)
        );
        assert_eq!(
            tracker.observe(&parse("@batch=missing BATCH +b netjoin")),
            Err(Error::UnknownParent)
        );
        assert!(tracker.classify(&parse("@batch=a CMD")).is_replay);
        tracker.observe(&parse("BATCH -missing")).unwrap();
        assert_eq!(tracker.len(), 1);
        let mut injected = parse("BATCH +b netjoin");
        injected.params[1] = "a\r\nQUIT".into();
        assert_eq!(
            tracker.observe(&injected),
            Err(Error::Wire(irc::Error::IllegalByte))
        );
        assert_eq!(tracker.len(), 1);
        let oversized = parse(&format!("BATCH +b netjoin :{}", "x".repeat(512)));
        assert_eq!(
            tracker.observe(&oversized),
            Err(Error::Wire(irc::Error::MessageTooLong))
        );
        assert_eq!(tracker.len(), 1);
    }

    #[test]
    fn bounded_map_child_close_and_reuse() {
        let mut tracker = BatchTracker::new();
        for i in 0..MAX_OPEN_BATCHES {
            tracker
                .observe(&parse(&format!("BATCH +b{i} netjoin")))
                .unwrap();
        }
        assert_eq!(tracker.len(), MAX_OPEN_BATCHES);
        assert_eq!(
            tracker.observe(&parse("BATCH +overflow netjoin")),
            Err(Error::TooManyBatches)
        );
        tracker.observe(&parse("BATCH -b0")).unwrap();
        tracker
            .observe(&parse("@batch=b1 BATCH +child chathistory"))
            .unwrap();
        tracker.observe(&parse("@batch=b1 BATCH -child")).unwrap();
        assert_eq!(tracker.len(), MAX_OPEN_BATCHES - 1);
        assert!(
            tracker
                .classify(&parse("@batch=b1 CMD"))
                .batch_type
                .is_some()
        );
        tracker.observe(&parse("batch +child netjoin")).unwrap();
        assert_eq!(tracker.len(), MAX_OPEN_BATCHES);
    }

    #[test]
    fn requests_and_server_limits() {
        let requests = HistoryRequests::new(Some("50"));
        for (message, line) in [
            (
                requests.latest("#chan", 100).unwrap(),
                "CHATHISTORY LATEST #chan * 50\r\n",
            ),
            (
                requests.before("#chan", "msgid=abc", 10).unwrap(),
                "CHATHISTORY BEFORE #chan msgid=abc 10\r\n",
            ),
            (
                requests
                    .after("nick", "timestamp=2026-01-01T00:00:00.000Z", 0)
                    .unwrap(),
                "CHATHISTORY AFTER nick timestamp=2026-01-01T00:00:00.000Z 1\r\n",
            ),
            (
                requests
                    .targets("2026-01-02T00:00:00.000Z", "2026-01-01T00:00:00.000Z", -10)
                    .unwrap(),
                "CHATHISTORY TARGETS timestamp=2026-01-02T00:00:00.000Z timestamp=2026-01-01T00:00:00.000Z 1\r\n",
            ),
        ] {
            assert_eq!(message.serialize().unwrap(), line);
            assert_eq!(Message::parse(line).unwrap(), message);
        }
        for value in [
            None,
            Some(""),
            Some("no"),
            Some("0"),
            Some("-1"),
            Some("99999999999999999999999999999999"),
        ] {
            assert_eq!(HistoryRequests::new(value).limit(100), 100);
        }
        assert_eq!(requests.limit(i64::MIN), 1);
        assert_eq!(requests.limit(i64::MAX), 50);
        assert_eq!(HistoryRequests::new(Some("1")).limit(50), 1);
    }

    #[test]
    fn request_injection_and_length_rejection() {
        let requests = HistoryRequests::default();
        for value in ["", "a b", ":x", "x\r\nQUIT", "a\0b", "a\tb"] {
            assert!(requests.latest(value, 10).is_err());
            assert!(
                requests
                    .before("#chan", &format!("msgid={value}"), 10)
                    .is_err()
            );
            assert!(
                requests
                    .after("#chan", &format!("timestamp={value}"), 10)
                    .is_err()
            );
            assert!(requests.targets(value, "2026-01-01T00:00:00Z", 10).is_err());
        }
        for reference in ["*", "abc", "msgid=", "timestamp=", "other=abc"] {
            assert_eq!(
                requests.before("#chan", reference, 10),
                Err(Error::InvalidRequest)
            );
        }
        assert_eq!(
            requests.latest(&"x".repeat(512), 1),
            Err(Error::Wire(irc::Error::MessageTooLong))
        );
        assert_eq!(
            requests.targets(&"x".repeat(512), "2026-01-01T00:00:00Z", 1),
            Err(Error::Wire(irc::Error::MessageTooLong))
        );
    }
}
