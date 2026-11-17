//! Pure IRCv3 CAP negotiation, independent of transport and wire message types.
//!
//! Adapted from Lurk's `cap/` (<https://github.com/exec/lurk>).
//! Copyright (c) 2026 exec. MIT licensed; see the repository LICENSE.
//! The owner feeds parsed command parameters, sends returned actions, and runs
//! SASL separately while [`Negotiator::need_sasl`] is true.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_CAPS: usize = 1024;
const MAX_LIST_BYTES: usize = 8192;
const MAX_TOKEN_BYTES: usize = 4096;
// Classic 512-byte IRC limit, including "CAP REQ :" and CRLF.
const REQUEST_BUDGET: usize = 501;

/// A command the caller should send; no action performs I/O.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    /// Begin CAP version 302 negotiation.
    Ls302,
    /// Request these capabilities, in deterministic caller preference order.
    Request(Vec<String>),
    /// Release the registration hold.
    End,
}

impl Action {
    /// Formats the command without CRLF.
    ///
    /// Actions returned by the negotiator are injection-free and at most 510
    /// bytes. Manually constructed `Request` values must be validated by the
    /// caller before sending, just like any other outbound command.
    pub fn to_wire(&self) -> String {
        match self {
            Self::Ls302 => "CAP LS 302".into(),
            Self::Request(caps) => format!("CAP REQ :{}", caps.join(" ")),
            Self::End => "CAP END".into(),
        }
    }
}

/// Current initial-registration negotiation phase.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum State {
    /// Not started.
    #[default]
    Init,
    /// Collecting a potentially multiline CAP LS reply.
    Listing,
    /// Waiting for one ACK or NAK per emitted CAP REQ line.
    Requesting,
    /// SASL was acknowledged; the caller must finish its authentication exchange.
    WaitingSasl,
    /// CAP END has been emitted; NEW/DEL/ACK/NAK/LIST remain supported.
    Done,
}

/// One connection's capability state machine.
///
/// Maps, wanted names, and in-flight names are bounded to 1024 entries. Lists
/// over 8192 bytes and tokens over 4096 bytes are ignored. Names which cannot
/// fit a classic IRC request line are ignored, never emitted oversized.
/// Duplicate NEW notifications do not grow pending requests. Handshake timeout,
/// SASL failure policy, and reconnect/reset (construct a fresh value) belong to
/// the caller. Ownership replaces Lurk's mutex: mutation requires `&mut self`.
#[derive(Clone, Debug)]
pub struct Negotiator {
    wanted: Vec<String>,
    wanted_set: BTreeSet<String>,
    available: BTreeMap<String, String>,
    enabled: BTreeSet<String>,
    pending: VecDeque<Vec<String>>,
    state: State,
    collecting_list: bool,
    sasl_acked: bool,
    sasl_done: bool,
}

impl Negotiator {
    /// Copies wanted names, ignoring duplicates, empty/invalid names and overflow.
    pub fn new(wanted: &[&str]) -> Self {
        let mut n = Self {
            wanted: Vec::new(),
            wanted_set: BTreeSet::new(),
            available: BTreeMap::new(),
            enabled: BTreeSet::new(),
            pending: VecDeque::new(),
            state: State::Init,
            collecting_list: false,
            sasl_acked: false,
            sasl_done: false,
        };
        for name in wanted.iter().map(|s| s.trim()) {
            if valid_name(name) && n.wanted.len() < MAX_CAPS && n.wanted_set.insert(name.into()) {
                n.wanted.push(name.into());
            }
        }
        n
    }

    /// Starts negotiation once; subsequent calls return no actions.
    pub fn start(&mut self) -> Vec<Action> {
        if self.state != State::Init {
            return Vec::new();
        }
        self.state = State::Listing;
        vec![Action::Ls302]
    }

    /// Current phase; post-registration requests leave this at `Done`.
    pub fn state(&self) -> State {
        self.state
    }

    /// Advertised names and values (empty for value-less capabilities).
    pub fn available(&self) -> &BTreeMap<String, String> {
        &self.available
    }

    /// Enabled capabilities in sorted order.
    pub fn enabled(&self) -> &BTreeSet<String> {
        &self.enabled
    }

    /// Whether a capability is currently enabled.
    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled.contains(name)
    }

    /// True only after all initial verdicts arrive and SASL must run.
    pub fn need_sasl(&self) -> bool {
        self.state == State::WaitingSasl
    }

    /// Advertised SASL mechanisms in server order; empty means use caller policy.
    pub fn sasl_mechanisms(&self) -> Vec<&str> {
        self.available
            .get("sasl")
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Completes the SASL hold regardless of success; failure policy is external.
    /// Calling outside `WaitingSasl` is a no-op.
    pub fn sasl_complete(&mut self) -> Vec<Action> {
        if !self.need_sasl() {
            return Vec::new();
        }
        self.sasl_done = true;
        self.finish()
    }

    /// Consumes a command and parameters, returning outbound actions.
    ///
    /// CAP parameters are `[target, subcommand, caps]` or, for LS/LIST
    /// continuation, `[target, subcommand, "*", caps]`. Trailing parameter
    /// colons must already be removed by the parser. Non-CAP, malformed, and
    /// unknown subcommands are ignored. The target `"*"` is not continuation.
    /// NAK resolves one entire request even when its echoed list is truncated.
    pub fn receive(&mut self, command: &str, params: &[&str]) -> Vec<Action> {
        if !command.eq_ignore_ascii_case("CAP") || !(3..=4).contains(&params.len()) {
            return Vec::new();
        }
        let more = params.len() == 4 && params[2] == "*";
        if params.len() == 4 && !more {
            return Vec::new();
        }
        let list = params[params.len() - 1];
        if list.len() > MAX_LIST_BYTES {
            return Vec::new();
        }
        let sub = params[1];
        if more && !sub.eq_ignore_ascii_case("LS") && !sub.eq_ignore_ascii_case("LIST") {
            return Vec::new();
        }
        if sub.eq_ignore_ascii_case("LS") {
            if self.state != State::Listing {
                return Vec::new();
            }
            self.advertise(list);
            if more {
                return Vec::new();
            }
            let actions = self.request_available(None);
            if actions.is_empty() {
                return self.finish();
            }
            self.state = State::Requesting;
            return actions;
        }
        if sub.eq_ignore_ascii_case("NEW") {
            self.advertise(list);
            if self.state == State::Done {
                return self.request_available(Some(list));
            }
        } else if sub.eq_ignore_ascii_case("DEL") {
            for (name, _) in cap_tokens(list) {
                self.available.remove(name);
                self.enabled.remove(name);
                if name == "sasl" {
                    self.sasl_acked = false;
                }
            }
            if self.need_sasl() && !self.sasl_acked {
                return self.finish();
            }
        } else if sub.eq_ignore_ascii_case("LIST") {
            if !self.collecting_list {
                self.enabled.clear();
            }
            for (name, _) in cap_tokens(list) {
                self.enable(name);
            }
            self.collecting_list = more;
        } else if sub.eq_ignore_ascii_case("ACK") || sub.eq_ignore_ascii_case("NAK") {
            if sub.eq_ignore_ascii_case("ACK") {
                for (name, _) in cap_tokens(list) {
                    if let Some(name) = name.strip_prefix('-') {
                        self.enabled.remove(name);
                        if name == "sasl" {
                            self.sasl_acked = false;
                        }
                    } else if self.wanted_set.contains(name) {
                        self.enable(name);
                        if name == "sasl" {
                            self.sasl_acked = true;
                        }
                    }
                }
            }
            // Accounting is per REQ line, not capability: NAK can be truncated.
            if self.pending.pop_front().is_none() {
                return Vec::new();
            }
            if self.state == State::Done || !self.pending.is_empty() {
                return Vec::new();
            }
            if self.sasl_acked && !self.sasl_done && self.wanted_set.contains("sasl") {
                self.state = State::WaitingSasl;
            } else {
                return self.finish();
            }
        }
        Vec::new()
    }

    fn advertise(&mut self, list: &str) {
        for (name, value) in cap_tokens(list) {
            if valid_name(name)
                && (self.available.contains_key(name) || self.available.len() < MAX_CAPS)
            {
                self.available.insert(name.into(), value.into());
            }
        }
    }

    fn enable(&mut self, name: &str) {
        if valid_name(name) && self.enabled.len() < MAX_CAPS {
            self.enabled.insert(name.into());
        }
    }

    fn request_available(&mut self, offered: Option<&str>) -> Vec<Action> {
        let pending: BTreeSet<_> = self.pending.iter().flatten().map(String::as_str).collect();
        let offered: Option<BTreeSet<_>> =
            offered.map(|list| cap_tokens(list).map(|(name, _)| name).collect());
        let names: Vec<_> = self
            .wanted
            .iter()
            .filter(|name| {
                self.available.contains_key(*name)
                    && !self.enabled.contains(*name)
                    && !pending.contains(name.as_str())
                    && offered
                        .as_ref()
                        .is_none_or(|set| set.contains(name.as_str()))
            })
            .cloned()
            .collect();
        let mut actions = Vec::new();
        let mut current = Vec::new();
        let mut bytes = 0;
        for name in names {
            let added = name.len() + usize::from(!current.is_empty());
            if bytes + added > REQUEST_BUDGET {
                self.pending.push_back(current.clone());
                actions.push(Action::Request(std::mem::take(&mut current)));
                bytes = 0;
            }
            bytes += name.len() + usize::from(!current.is_empty());
            current.push(name);
        }
        if !current.is_empty() {
            self.pending.push_back(current.clone());
            actions.push(Action::Request(current));
        }
        actions
    }

    fn finish(&mut self) -> Vec<Action> {
        if self.state == State::Done {
            return Vec::new();
        }
        self.state = State::Done;
        vec![Action::End]
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= REQUEST_BUDGET
        && !name.starts_with(['-', ':', '='])
        && name.bytes().all(|b| b.is_ascii_graphic() && b != b'=')
}

fn cap_tokens(list: &str) -> impl Iterator<Item = (&str, &str)> {
    list.split_ascii_whitespace()
        .filter(|s| s.len() <= MAX_TOKEN_BYTES)
        .filter_map(|s| {
            let (name, value) = s.split_once('=').unwrap_or((s, ""));
            let bare = name.strip_prefix('-').unwrap_or(name);
            (valid_name(bare) && !value.bytes().any(|b| b.is_ascii_control()))
                .then_some((name, value))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(n: &mut Negotiator, sub: &str, caps: &str) -> Vec<String> {
        n.receive("CAP", &["*", sub, caps])
            .iter()
            .map(Action::to_wire)
            .collect()
    }

    #[test]
    fn full_exchange_and_sasl_hold() {
        let mut n = Negotiator::new(&["server-time", "sasl", "sasl", "", "missing"]);
        assert_eq!(n.start(), [Action::Ls302]);
        assert!(n.start().is_empty());
        assert!(n.sasl_complete().is_empty());
        assert!(
            n.receive("CAP", &["*", "LS", "*", "server-time"])
                .is_empty()
        );
        assert_eq!(n.state(), State::Listing);
        assert_eq!(
            feed(&mut n, "LS", "sasl=PLAIN,EXTERNAL other=a=b"),
            ["CAP REQ :server-time sasl"]
        );
        assert_eq!(n.available().get("other").map(String::as_str), Some("a=b"));
        assert_eq!(n.sasl_mechanisms(), ["PLAIN", "EXTERNAL"]);
        assert!(feed(&mut n, "ACK", "server-time sasl unrequested").is_empty());
        assert!(!n.is_enabled("unrequested"));
        assert!(n.need_sasl());
        assert_eq!(n.sasl_complete(), [Action::End]);
        assert!(n.sasl_complete().is_empty());
        assert_eq!(n.state(), State::Done);
        assert!(feed(&mut n, "LS", "sasl").is_empty());
    }

    #[test]
    fn no_intersection_and_nak_finish() {
        let mut n = Negotiator::new(&["wanted"]);
        n.start();
        assert_eq!(feed(&mut n, "LS", "unwanted"), ["CAP END"]);
        let mut n = Negotiator::new(&["wanted"]);
        n.start();
        assert_eq!(feed(&mut n, "LS", "wanted"), ["CAP REQ :wanted"]);
        assert_eq!(feed(&mut n, "NAK", "wan"), ["CAP END"]);
        assert!(!n.is_enabled("wanted"));
        assert!(feed(&mut n, "NAK", "").is_empty());
    }

    #[test]
    fn dynamic_caps_disable_and_list() {
        let mut n = Negotiator::new(&["echo-message", "server-time"]);
        n.start();
        feed(&mut n, "LS", "");
        assert_eq!(
            feed(&mut n, "NEW", "echo-message echo-message"),
            ["CAP REQ :echo-message"]
        );
        assert!(feed(&mut n, "NEW", "echo-message").is_empty());
        assert!(feed(&mut n, "ACK", "echo-message").is_empty());
        assert!(n.is_enabled("echo-message"));
        feed(&mut n, "ACK", "-echo-message");
        assert!(!n.is_enabled("echo-message"));
        feed(&mut n, "ACK", "echo-message");
        feed(&mut n, "DEL", "echo-message");
        assert!(!n.is_enabled("echo-message"));
        assert!(!n.available().contains_key("echo-message"));
        n.receive("CAP", &["*", "LIST", "*", "server-time"]);
        n.receive("CAP", &["*", "LIST", "multi-prefix"]);
        assert_eq!(
            n.enabled().iter().map(String::as_str).collect::<Vec<_>>(),
            ["multi-prefix", "server-time"]
        );
        feed(&mut n, "LIST", "");
        assert!(n.enabled().is_empty());
    }

    #[test]
    fn split_requests_and_truncated_nak_accounting() {
        let names: Vec<_> = (0..30)
            .map(|i| format!("cap-{i:03}-{}", "a".repeat(80)))
            .collect();
        let refs: Vec<_> = names.iter().map(String::as_str).collect();
        let mut n = Negotiator::new(&refs);
        n.start();
        let actions = feed(&mut n, "LS", &names.join(" "));
        assert!(actions.len() > 1);
        assert!(actions.iter().all(|line| line.len() + 2 <= 512));
        for _ in 1..actions.len() {
            assert!(feed(&mut n, "NAK", "truncated").is_empty());
            assert_eq!(n.state(), State::Requesting);
        }
        assert_eq!(feed(&mut n, "NAK", "truncated"), ["CAP END"]);
    }

    #[test]
    fn unsolicited_verdicts_do_not_advance_phases() {
        let mut n = Negotiator::new(&["sasl"]);
        n.start();
        assert!(feed(&mut n, "ACK", "unknown").is_empty());
        assert_eq!(n.state(), State::Listing);
        assert!(!n.is_enabled("unknown"));
        feed(&mut n, "LS", "sasl");
        feed(&mut n, "ACK", "sasl");
        assert!(n.need_sasl());
        assert!(feed(&mut n, "NAK", "").is_empty());
        assert!(n.need_sasl());
        assert_eq!(feed(&mut n, "DEL", "sasl"), ["CAP END"]);
    }

    #[test]
    fn new_only_requests_newly_advertised_names() {
        let mut n = Negotiator::new(&["rejected", "fresh"]);
        n.start();
        feed(&mut n, "LS", "rejected");
        assert_eq!(feed(&mut n, "NAK", "rej"), ["CAP END"]);
        assert!(feed(&mut n, "NEW", "unwanted").is_empty());
        assert_eq!(feed(&mut n, "NEW", "fresh"), ["CAP REQ :fresh"]);
        assert!(feed(&mut n, "NEW", "fresh").is_empty());
        assert!(feed(&mut n, "ACK", "fresh").is_empty());
        assert_eq!(n.state(), State::Done);
        assert_eq!(feed(&mut n, "NEW", "rejected"), ["CAP REQ :rejected"]);
    }

    #[test]
    fn sasl_waits_for_every_request_verdict() {
        let long = "a".repeat(REQUEST_BUDGET);
        let mut n = Negotiator::new(&["sasl", &long]);
        n.start();
        let actions = feed(&mut n, "LS", &format!("sasl {long}"));
        assert_eq!(actions.len(), 2);
        assert!(feed(&mut n, "ACK", "sasl").is_empty());
        assert!(!n.need_sasl());
        assert!(n.sasl_complete().is_empty());
        assert!(feed(&mut n, "NAK", "truncated").is_empty());
        assert!(n.need_sasl());
        assert!(n.sasl_mechanisms().is_empty());
        assert_eq!(n.sasl_complete(), [Action::End]);
    }

    #[test]
    fn values_and_sasl_rejection() {
        let mut n = Negotiator::new(&["server-time", "sasl"]);
        n.start();
        feed(&mut n, "LS", "server-time sasl");
        assert_eq!(feed(&mut n, "NAK", "server-time sasl"), ["CAP END"]);
        assert!(!n.need_sasl());
        assert!(n.enabled().is_empty());
        // Case-insensitive commands/subcommands, case-sensitive capability names.
        assert!(n.receive("cap", &["nick", "new", "SERVER-TIME"]).is_empty());
        assert_eq!(n.receive("cap", &["nick", "new", "server-time"]).len(), 1);
    }

    #[test]
    fn dynamic_flood_bounds_pending_and_available_sets() {
        let mut n = Negotiator::new(&["wanted"]);
        n.start();
        feed(&mut n, "LS", "");
        for _ in 0..2000 {
            feed(&mut n, "NEW", "wanted");
        }
        assert_eq!(n.pending.len(), 1);
        for i in 0..MAX_CAPS + 20 {
            feed(&mut n, "NEW", &format!("cap{i}"));
        }
        assert_eq!(n.available().len(), MAX_CAPS);
        assert_eq!(n.pending.len(), 1);
        feed(&mut n, "NEW", "wanted=new");
        assert_eq!(n.available().get("wanted").map(String::as_str), Some("new"));
        feed(&mut n, "DEL", "cap0");
        feed(&mut n, "NEW", "replacement");
        assert!(n.available().contains_key("replacement"));
        assert_eq!(n.available().len(), MAX_CAPS);
    }

    #[test]
    fn hostile_input_is_bounded_and_never_injected() {
        let huge = "x".repeat(REQUEST_BUDGET + 1);
        let mut n = Negotiator::new(&["ok", "bad\r\nQUIT", ":bad", "-bad", "a=b", &huge]);
        n.start();
        assert_eq!(n.wanted, ["ok"]);
        for i in 0..MAX_CAPS + 20 {
            n.receive("CAP", &["*", "LS", "*", &format!("cap{i}")]);
        }
        assert_eq!(n.available().len(), MAX_CAPS);
        n.receive("CAP", &["*", "LS", "*", "cap0=new"]);
        assert_eq!(n.available().get("cap0").map(String::as_str), Some("new"));
        n.receive("CAP", &["*", "LS", ""]);
        for i in 0..MAX_CAPS + 20 {
            n.receive("CAP", &["*", "LIST", "*", &format!("enabled{i}")]);
        }
        assert_eq!(n.enabled().len(), MAX_CAPS);
        let before = n.available().len();
        feed(&mut n, "NEW", &"z".repeat(MAX_LIST_BYTES + 1));
        assert_eq!(n.available().len(), before);
        for params in [
            &[][..],
            &["*"][..],
            &["*", "LS"][..],
            &["*", "LS", "bad", "caps"][..],
        ] {
            assert!(n.receive("CAP", params).is_empty());
        }
        assert!(n.receive("PING", &["*", "LS", "caps"]).is_empty());
    }
}
