//! Pure desktop state driven by synthetic events, without a window or network.

use ruckus_core::{client::SendId, commands, isupport::CaseMapping};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const HISTORY_LIMIT: usize = 500;
const BUFFER_LIMIT: usize = 64;
const PENDING_SEND_LIMIT: usize = 512;
// NUL is forbidden on the wire, so this key cannot collide with a legal nick.
pub(crate) const STATUS: &str = "\0Network";

#[derive(Default)]
pub(crate) struct Buffer {
    pub messages: VecDeque<String>,
    pub revision: u64,
    /// In-place status edits are not history appends.
    pub edits: u64,
}

struct PendingSend {
    target: String,
    line: String,
    sent_line: String,
    unconfirmed_line: String,
}

pub(crate) struct Model {
    pub buffers: BTreeMap<String, Buffer>,
    pub selected: String,
    pub nick: String,
    pub status: String,
    pub connected: bool,
    case_mapping: CaseMapping,
    chan_types: String,
    status_msg: String,
    pending_sends: BTreeMap<SendId, PendingSend>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Command {
    Join(String),
    Part(String),
    Message { target: String, text: String },
    Nick(String),
    Disconnect,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            buffers: BTreeMap::from([(STATUS.into(), Buffer::default())]),
            selected: STATUS.into(),
            nick: String::new(),
            status: "Disconnected".into(),
            connected: false,
            case_mapping: CaseMapping::default(),
            chan_types: "#&".into(),
            status_msg: String::new(),
            pending_sends: BTreeMap::new(),
        }
    }
}

impl Model {
    pub fn selected_revision(&self) -> u64 {
        self.buffers
            .get(&self.selected)
            .map(|buffer| buffer.revision)
            .unwrap_or(0)
    }

    pub fn sync_snapshot(
        &mut self,
        state: &ruckus_core::state::State,
        registered: bool,
        closed: bool,
    ) {
        self.set_case_mapping(state.features.case_mapping());
        self.chan_types = state.features.chan_types().into();
        self.status_msg = state.features.status_msg().into();
        self.nick = state.nick.clone();
        self.connected = registered && !closed;
        if closed {
            self.status = "Disconnected".into();
        } else if registered {
            self.status = "Connected".into();
            let joined: BTreeSet<_> = state
                .channels
                .values()
                .map(|channel| self.case_mapping.fold(&channel.name))
                .collect();
            for channel in state.channels.values() {
                if self.existing_target(&channel.name).is_some() {
                    continue;
                }
                if self.buffers.len() >= BUFFER_LIMIT {
                    // Confirmed joined channels take priority over PMs and
                    // retained parted-channel history. Never evict another
                    // joined channel or the status buffer.
                    let evict = self
                        .buffers
                        .keys()
                        .filter(|key| {
                            key.as_str() != STATUS && !joined.contains(&self.case_mapping.fold(key))
                        })
                        .min_by_key(|key| key.as_str() == self.selected)
                        .cloned();
                    if let Some(evict) = evict {
                        self.buffers.remove(&evict);
                        if self.selected == evict {
                            self.selected = STATUS.into();
                        }
                        self.network_notice(
                            "Retained conversation history evicted for a joined channel.",
                        );
                    }
                }
                // Core bounds joined channels to 256; when every buffer is
                // joined, allow that bounded set plus the status buffer rather
                // than hiding an active channel behind the private-history cap.
                self.buffers.entry(channel.name.clone()).or_default();
            }
        } else {
            self.status = "Connecting…".into();
        }
    }

    pub(crate) fn is_channel(&self, target: &str) -> bool {
        target
            .chars()
            .next()
            .is_some_and(|prefix| self.chan_types.contains(prefix))
    }

    fn channel_target<'a>(&self, target: &'a str) -> Option<&'a str> {
        let bare = target.trim_start_matches(|ch| self.status_msg.contains(ch));
        if bare != target && self.is_channel(bare) {
            Some(bare)
        } else {
            self.is_channel(target).then_some(target)
        }
    }

    fn push_existing(&mut self, target: &str, text: impl AsRef<str>) {
        if self.existing_target(target).is_some() {
            self.push(target, text);
        }
    }

    fn existing_target(&self, target: &str) -> Option<String> {
        if target == STATUS {
            return Some(STATUS.into());
        }
        let folded = self.case_mapping.fold(target);
        self.buffers
            .keys()
            .find(|key| key.as_str() != STATUS && self.case_mapping.fold(key) == folded)
            .cloned()
    }

    pub fn select(&mut self, target: &str) {
        if let Some(target) = self.existing_target(target) {
            self.selected = target;
        }
    }

    pub fn set_case_mapping(&mut self, mapping: CaseMapping) {
        if mapping == self.case_mapping {
            return;
        }
        self.case_mapping = mapping;
        let selected = self.selected.clone();
        let buffers = std::mem::take(&mut self.buffers);
        for (target, buffer) in buffers {
            // Preserve the already-admitted joined-channel set even when it
            // exceeds the private/history cap. Only fold collisions merge it.
            if self.existing_target(&target).is_none() {
                self.buffers.entry(target.clone()).or_default();
            }
            for message in buffer.messages {
                self.push(&target, message);
            }
        }
        self.buffers.entry(STATUS.into()).or_default();
        self.select(&selected);
        if !self.buffers.contains_key(&self.selected) {
            self.selected = STATUS.into();
        }
    }

    pub fn push(&mut self, target: &str, text: impl AsRef<str>) {
        let existing = self.existing_target(target);
        let target = if let Some(existing) = existing {
            existing
        } else if self.buffers.len() < BUFFER_LIMIT {
            target.to_owned()
        } else {
            STATUS.to_owned()
        };
        let buffer = self.buffers.entry(target).or_default();
        if buffer.messages.len() >= HISTORY_LIMIT {
            buffer.messages.pop_front();
        }
        buffer.messages.push_back(clean(text.as_ref()));
        buffer.revision = buffer.revision.wrapping_add(1);
    }

    pub fn network_notice(&mut self, text: impl AsRef<str>) {
        self.push(STATUS, text);
    }

    pub fn can_track_send(&self) -> bool {
        self.pending_sends.len() < PENDING_SEND_LIMIT
    }

    pub fn queued(&mut self, id: SendId, target: &str, text: &str) {
        let line = clean(&format!("{} (queued): {text}", self.nick));
        let sent_line = clean(&format!("{} (sent): {text}", self.nick));
        let unconfirmed_line = clean(&format!("{} (unconfirmed): {text}", self.nick));
        if !self.can_track_send() {
            self.push(target, unconfirmed_line);
            return;
        }
        self.push(target, &line);
        // push may redirect to the status buffer at the conversation cap.
        let target = self
            .existing_target(target)
            .unwrap_or_else(|| STATUS.into());
        self.pending_sends.insert(
            id,
            PendingSend {
                target,
                line,
                sent_line,
                unconfirmed_line,
            },
        );
    }

    fn finish_send(&mut self, id: SendId, written: bool) {
        let Some(pending) = self.pending_sends.remove(&id) else {
            return;
        };
        let Some(target) = self.existing_target(&pending.target) else {
            return;
        };
        let Some(buffer) = self.buffers.get_mut(&target) else {
            return;
        };
        if let Some(line) = buffer
            .messages
            .iter_mut()
            .find(|line| **line == pending.line)
        {
            *line = if written {
                pending.sent_line
            } else {
                pending.unconfirmed_line
            };
            buffer.edits = buffer.edits.wrapping_add(1);
        }
    }

    pub fn sent(&mut self, id: SendId) {
        self.finish_send(id, true);
    }

    pub fn unconfirm_pending(&mut self) {
        let ids: Vec<_> = self.pending_sends.keys().copied().collect();
        for id in ids {
            self.finish_send(id, false);
        }
    }

    pub fn registered(&mut self, nick: String) {
        self.connected = true;
        self.status = "Connected".into();
        self.nick = nick;
        self.network_notice("Registration complete.");
    }

    pub fn disconnected(&mut self, reason: &str) {
        self.unconfirm_pending();
        self.connected = false;
        self.status = "Disconnected".into();
        self.network_notice(format!("Disconnected: {reason}"));
    }

    /// Parse frontend syntax using core action builders for wire validation.
    /// Zero relay reservation checks unprefixed feasibility here; the client
    /// still owns the live sender-prefix budget and outbound queue capacity.
    pub fn command(&self, input: &str) -> Result<Command, &'static str> {
        if !self.connected {
            return Err("Connect before sending messages.");
        }
        if input.is_empty() || input.contains(['\r', '\n', '\0']) {
            return Err("Enter a single nonempty line.");
        }
        if let Some(command) = input.strip_prefix('/') {
            let (verb, rest) = command.split_once(' ').unwrap_or((command, ""));
            let rest = rest.trim();
            return match verb.to_ascii_lowercase().as_str() {
                "join" if self.is_channel(rest) && commands::join(&[rest], &[]).is_ok() => {
                    Ok(Command::Join(rest.into()))
                }
                "part"
                    if rest.is_empty()
                        && self.is_channel(&self.selected)
                        && commands::part(&[&self.selected], None).is_ok() =>
                {
                    Ok(Command::Part(self.selected.clone()))
                }
                "part" if self.is_channel(rest) && commands::part(&[rest], None).is_ok() => {
                    Ok(Command::Part(rest.into()))
                }
                "nick" if commands::nick(rest).is_ok() => Ok(Command::Nick(rest.into())),
                "quit" if rest.is_empty() => Ok(Command::Disconnect),
                "msg" => {
                    let (target, text) =
                        rest.split_once(' ').ok_or("Usage: /msg nickname message")?;
                    if text.trim().is_empty() || commands::privmsg(target, text, 0).is_err() {
                        return Err("Usage: /msg nickname message");
                    }
                    Ok(Command::Message {
                        target: target.into(),
                        text: text.into(),
                    })
                }
                _ => Err(
                    "Commands: /join #channel, /part [#channel], /msg nick text, /nick nick, /quit",
                ),
            };
        }
        if self.selected == STATUS {
            return Err("Select a conversation or use /join #channel.");
        }
        commands::privmsg(&self.selected, input, 0)
            .map_err(|_| "Invalid message target or IRC message length.")?;
        Ok(Command::Message {
            target: self.selected.clone(),
            text: input.into(),
        })
    }

    pub fn receive_message(
        &mut self,
        message: &ruckus_core::irc::Message,
        affected_channels: &[String],
        self_nick: &str,
    ) {
        // Snapshots can be ahead of retained FIFO events. Route this message
        // with the identity captured by the worker, then reconcile the latest
        // snapshot after draining the bounded event batch.
        self.nick = self_nick.into();
        let Some(source) = message
            .source
            .as_deref()
            .filter(|source| !source.is_empty())
        else {
            self.network_notice(format!("{} {}", message.command, message.params.join(" ")));
            return;
        };
        self.received(source, &message.command, &message.params);
        let (nick, _, _) = ruckus_core::state::split_mask(source);
        let text = match message.command.as_str() {
            "QUIT" => Some(format!(
                "← {nick} quit: {}",
                message.params.last().map(String::as_str).unwrap_or("")
            )),
            "NICK" => message
                .params
                .first()
                .map(|new| format!("{nick} is now known as {new}")),
            _ => None,
        };
        if let Some(text) = text {
            for channel in affected_channels {
                self.push_existing(channel, &text);
            }
        }
    }

    pub fn received(&mut self, source: &str, command: &str, params: &[String]) {
        let (nick, _, _) = ruckus_core::state::split_mask(source);
        let first = params.first().map(String::as_str).unwrap_or("");
        let last = params.last().map(String::as_str).unwrap_or("");
        match command {
            "PRIVMSG" | "NOTICE" if params.len() >= 2 && !first.is_empty() => {
                let target = if let Some(channel) = self.channel_target(first) {
                    channel
                } else if self.case_mapping.fold(nick) == self.case_mapping.fold(&self.nick) {
                    first
                } else {
                    nick
                };
                self.push(target, format!("<{nick}> {last}"));
            }
            "JOIN" if valid_target(first) && self.is_channel(first) => {
                if self.case_mapping.fold(nick) == self.case_mapping.fold(&self.nick) {
                    self.push(first, format!("→ {nick} joined"));
                    self.select(first);
                } else {
                    self.push_existing(first, format!("→ {nick} joined"));
                }
            }
            "PART" | "KICK"
                if valid_target(first)
                    && self.is_channel(first)
                    && (command != "KICK"
                        || params.get(1).is_some_and(|nick| valid_target(nick))) =>
            {
                let leaving = if command == "KICK" {
                    params.get(1).map(String::as_str).unwrap_or("")
                } else {
                    nick
                };
                self.push_existing(first, format!("← {leaving} left"));
            }
            "QUIT" => {
                self.network_notice(format!("← {nick} quit: {last}"));
            }
            "NICK" if !first.is_empty() => {
                if self.case_mapping.fold(nick) == self.case_mapping.fold(&self.nick) {
                    self.nick = first.into();
                }
                self.network_notice(format!("{nick} is now known as {first}"));
            }
            // Membership, topics, modes and casemapping come from Client::snapshot,
            // not a competing frontend implementation of the IRC state machine.
            "353" | "366" | "332" | "333" => {}
            _ => self.network_notice(format!("{command} {}", params.join(" "))),
        }
    }
}

pub(crate) fn clean(text: &str) -> String {
    ruckus_core::sanitize::sanitize_terminal(text)
        .chars()
        .filter(|c| !c.is_control())
        .take(4096)
        .collect()
}

pub(crate) fn should_follow(
    selection_changed: bool,
    selected_appended: bool,
    near_bottom: bool,
) -> bool {
    selection_changed || (selected_appended && near_bottom)
}

fn valid_target(target: &str) -> bool {
    !target.is_empty()
        && !target.chars().any(char::is_whitespace)
        && !target.contains([',', '\r', '\n', '\0'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracked_sends_advance_in_place_without_adding_history_or_losing_the_author() {
        let mut model = Model::default();
        model.registered("exec".into());
        model.queued(SendId(1), "#ruckus", "first");
        model.queued(SendId(2), "#ruckus", "second");
        let revision = model.buffers["#ruckus"].revision;
        model.sent(SendId(2));
        assert_eq!(model.buffers["#ruckus"].messages[0], "exec (queued): first");
        assert_eq!(model.buffers["#ruckus"].messages[1], "exec (sent): second");
        model.sent(SendId(1));
        model.sent(SendId(1)); // duplicate notification is harmless
        assert_eq!(model.buffers["#ruckus"].messages.len(), 2);
        assert_eq!(model.buffers["#ruckus"].messages[0], "exec (sent): first");
        assert_eq!(model.buffers["#ruckus"].revision, revision);
        assert_eq!(model.buffers["#ruckus"].edits, 2);
    }

    #[test]
    fn missing_confirmations_never_claim_delivery_and_pending_tracking_is_bounded() {
        let mut model = Model::default();
        model.registered("exec".into());
        model.queued(SendId(1), "#ruckus", "connection ended");
        model.disconnected("closed");
        assert_eq!(
            model.buffers["#ruckus"].messages[0],
            "exec (unconfirmed): connection ended"
        );
        assert!(model.pending_sends.is_empty());
        for id in 2..PENDING_SEND_LIMIT as u64 + 3 {
            model.queued(SendId(id), "#ruckus", "pending");
        }
        assert_eq!(model.pending_sends.len(), PENDING_SEND_LIMIT);
        assert!(!model.can_track_send());
        model.unconfirm_pending();
        assert!(model.can_track_send());
    }

    #[test]
    fn absent_source_stays_a_system_notice_without_an_invented_author() {
        let mut model = Model::default();
        let message = ruckus_core::irc::Message::new("NOTICE", vec!["*".into(), "hello".into()]);
        model.receive_message(&message, &[], "me");
        assert_eq!(model.buffers[STATUS].messages[0], "NOTICE * hello");
        assert!(!model.buffers.contains_key("server"));
    }

    #[test]
    fn negotiated_channel_classification_survives_disconnect() {
        let mut state = ruckus_core::state::State::new("me");
        state.features = state.features.merge(&["CHANTYPES=#+!"]);
        let mut model = Model::default();
        model.sync_snapshot(&state, true, false);
        model.disconnected("done");
        assert!(model.is_channel("+local"));
        assert!(model.is_channel("!safe"));
        assert!(!model.is_channel("peer"));
    }

    #[test]
    fn snapshot_recovers_registration_nick_and_channels_without_events() {
        let mut state = ruckus_core::state::State::new("Me");
        state.apply(&ruckus_core::irc::Message::parse(":Me!u@h JOIN #Rust").unwrap());
        state.apply(&ruckus_core::irc::Message::parse(":Me!u@h NICK NewMe").unwrap());
        let mut model = Model {
            status: "Connecting…".into(),
            ..Default::default()
        };
        model.sync_snapshot(&state, true, false);
        assert!(model.connected);
        assert_eq!(model.status, "Connected");
        assert_eq!(model.nick, "NewMe");
        assert!(model.buffers.contains_key("#Rust"));
        model.sync_snapshot(&state, false, true);
        assert!(!model.connected);
        assert_eq!(model.status, "Disconnected");
        assert!(model.command("/join #other").is_err());
    }

    #[test]
    fn negotiated_channel_and_status_prefixes_route_to_channel_buffers() {
        let mut state = ruckus_core::state::State::new("me");
        state.features = ruckus_core::isupport::ISupport::parse(&["CHANTYPES=+!", "STATUSMSG=@%"]);
        let mut model = Model::default();
        model.sync_snapshot(&state, true, false);
        model.received("me@host", "JOIN", &["+Rust".into()]);
        assert_eq!(model.selected, "+Rust");
        model.received(
            "friend@host",
            "PRIVMSG",
            &["@%+rust".into(), "hi ops".into()],
        );
        assert_eq!(
            model.buffers["+Rust"].messages.back().unwrap(),
            "<friend> hi ops"
        );
        assert!(!model.buffers.contains_key("friend@host"));
        assert_eq!(
            model.command("/join !other"),
            Ok(Command::Join("!other".into()))
        );
        assert!(model.command("/join #unsupported").is_err());
        state.features = ruckus_core::isupport::ISupport::parse(&["CHANTYPES=#&+", "STATUSMSG=+"]);
        model.sync_snapshot(&state, true, false);
        assert_eq!(model.channel_target("+#rust"), Some("#rust"));
        assert_eq!(model.channel_target("+local"), Some("+local"));
    }

    #[test]
    fn unrelated_membership_events_cannot_exhaust_conversation_slots() {
        let mut model = Model::default();
        model.registered("me".into());
        for index in 0..100 {
            let target = format!("#fake{index}");
            model.received("other!u@h", "JOIN", std::slice::from_ref(&target));
            model.received("other!u@h", "PART", std::slice::from_ref(&target));
            model.received("other!u@h", "KICK", &[target, "victim".into()]);
        }
        assert_eq!(model.buffers.len(), 1);
        model.received("me!u@h", "JOIN", &["#real".into()]);
        model.received("other!u@h", "JOIN", &["#REAL".into()]);
        assert_eq!(model.buffers.len(), 2);
        assert_eq!(model.buffers["#real"].messages.len(), 2);
        model.received("other!u@h", "KICK", &["#real".into()]);
        assert_eq!(model.buffers["#real"].messages.len(), 2);
    }

    #[test]
    fn scrolling_follows_only_selected_appends_near_the_bottom() {
        assert!(!should_follow(false, false, true));
        assert!(!should_follow(false, true, false));
        assert!(should_follow(false, true, true));
        assert!(should_follow(true, false, false));
        let mut model = Model::default();
        model.push("#selected", "first");
        model.select("#selected");
        let revision = model.selected_revision();
        model.push("#other", "background");
        assert_eq!(model.selected_revision(), revision);
        for _ in 0..HISTORY_LIMIT {
            model.push("#selected", "append");
        }
        assert_ne!(model.selected_revision(), revision);
        model.disconnected("cancelled");
        assert!(model.command("draft after cancellation").is_err());
    }

    #[test]
    fn network_named_peer_does_not_collide_with_status_buffer() {
        let mut model = Model::default();
        model.registered("me".into());
        model.received("Network!u@h", "PRIVMSG", &["me".into(), "hello".into()]);
        model.select("Network");
        assert_ne!(model.selected, STATUS);
        assert!(model.buffers.contains_key("Network"));
        assert!(model.command("reply").is_ok());
    }

    #[test]
    fn quit_and_nick_context_reaches_shared_channel_histories() {
        let mut model = Model::default();
        model.registered("me".into());
        model.received("me!u@h", "JOIN", &["#a".into()]);
        model.received("me!u@h", "JOIN", &["#b".into()]);
        let message = ruckus_core::irc::Message::parse(":other!u@h QUIT :gone").unwrap();
        model.receive_message(&message, &["#a".into(), "#b".into()], "me");
        for channel in ["#a", "#b"] {
            assert!(model.buffers[channel].messages[1].contains("other quit: gone"));
        }
        let message = ruckus_core::irc::Message::parse(":other!u@h NICK renamed").unwrap();
        model.receive_message(&message, &["#a".into()], "me");
        assert!(model.buffers["#a"].messages[2].contains("renamed"));
    }

    #[test]
    fn event_identity_is_not_replaced_by_a_future_snapshot() {
        let mut model = Model::default();
        let mut state = ruckus_core::state::State::new("newnick");
        state.apply(&ruckus_core::irc::Message::parse(":newnick!u@h JOIN #rust").unwrap());
        model.sync_snapshot(&state, true, false);
        let echo =
            ruckus_core::irc::Message::parse(":oldnick!u@h PRIVMSG peer :sent earlier").unwrap();
        model.receive_message(&echo, &[], "oldnick");
        assert!(model.buffers.contains_key("peer"));
        assert!(!model.buffers.contains_key("oldnick"));
        let join = ruckus_core::irc::Message::parse(":oldnick!u@h JOIN #rust").unwrap();
        model.receive_message(&join, &[], "oldnick");
        assert_eq!(model.selected, "#rust");
        model.sync_snapshot(&state, true, false);
        assert_eq!(model.nick, "newnick");
    }

    #[test]
    fn joined_channels_take_priority_over_retained_private_histories() {
        let mut model = Model::default();
        for index in 0..BUFFER_LIMIT - 1 {
            model.push(&format!("peer{index}"), "retained private message");
        }
        assert_eq!(model.buffers.len(), BUFFER_LIMIT);
        let mut state = ruckus_core::state::State::new("me");
        for index in 0..ruckus_core::state::MAX_CHANNELS {
            let channel = format!("#joined{index}");
            state.apply(
                &ruckus_core::irc::Message::parse(&format!(":me!u@h JOIN {channel}")).unwrap(),
            );
            model.sync_snapshot(&state, true, false);
            model.select(&channel);
            assert_eq!(model.selected, channel);
        }
        assert_eq!(model.buffers.len(), ruckus_core::state::MAX_CHANNELS + 1);
        model.push("#joined0", "keep joined history");
        model.set_case_mapping(CaseMapping::Ascii);
        assert!(
            model.buffers["#joined0"]
                .messages
                .iter()
                .any(|line| line == "keep joined history")
        );
        assert_eq!(model.buffers.len(), ruckus_core::state::MAX_CHANNELS + 1);
    }

    #[test]
    fn disconnected_drafts_never_become_network_commands() {
        let model = Model::default();
        assert!(model.command("hello").is_err());
        assert!(!model.connected);
        assert!(model.buffers[STATUS].messages.is_empty());
    }

    #[test]
    fn commands_validate_wire_injection_and_targets() {
        let mut model = Model::default();
        model.registered("me".into());
        assert!(model.command("hello").is_err());
        assert_eq!(
            model.command("/join #rust"),
            Ok(Command::Join("#rust".into()))
        );
        assert!(model.command("/join #a,#b").is_err());
        assert!(model.command("/msg nick hello\r\nQUIT").is_err());
        model.selected = "#rust".into();
        assert_eq!(
            model.command("hello"),
            Ok(Command::Message {
                target: "#rust".into(),
                text: "hello".into()
            })
        );
        assert_eq!(model.command("/part"), Ok(Command::Part("#rust".into())));
    }

    #[test]
    fn frontend_commands_share_core_action_validation() {
        let mut model = Model::default();
        model.registered("me".into());
        for invalid in ["/nick :bad", "/msg :bad hello", "/join #a:b", "/part #a:b"] {
            assert!(
                model.command(invalid).is_err(),
                "{invalid} must be rejected"
            );
        }
        for target in [":bad", "a,b", "a\tb", "n".repeat(600).as_str()] {
            assert!(commands::nick(target).is_err());
            assert!(model.command(&format!("/nick {target}")).is_err());
            assert!(commands::privmsg(target, "hello", 0).is_err());
            assert!(model.command(&format!("/msg {target} hello")).is_err());
        }
        for target in ["#good", "#a:b", "#a,#b", "#a\tb"] {
            assert_eq!(
                model.command(&format!("/join {target}")).is_ok(),
                commands::join(&[target], &[]).is_ok(),
            );
            assert_eq!(
                model.command(&format!("/part {target}")).is_ok(),
                commands::part(&[target], None).is_ok(),
            );
        }
        model.selected = ":bad".into();
        assert!(model.command("ordinary composer text").is_err());
        model.selected = "#good".into();
        assert!(
            model
                .command(&"x".repeat(commands::MAX_TEXT_BYTES + 1))
                .is_err()
        );
        let mut state = ruckus_core::state::State::new("me");
        state.features = state.features.merge(&["CHANTYPES=#~"]);
        model.sync_snapshot(&state, true, false);
        assert_eq!(
            model.command("/join ~custom"),
            Ok(Command::Join("~custom".into()))
        );
        assert_eq!(
            model.command("/part ~custom"),
            Ok(Command::Part("~custom".into()))
        );
        assert!(model.command("/join &unsupported").is_err());
    }

    #[test]
    fn synthetic_events_update_conversations_and_nick() {
        let mut model = Model::default();
        model.registered("me".into());
        model.received("me!u@h", "JOIN", &["#rust".into()]);
        assert_eq!(model.selected, "#rust");
        model.received(
            "server",
            "353",
            &[
                "me".into(),
                "=".into(),
                "#rust".into(),
                "@me +friend".into(),
            ],
        );
        model.received("friend!u@h", "NICK", &["other".into()]);
        model.received("me!u@h", "NICK", &["newme".into()]);
        assert_eq!(model.nick, "newme");
        model.received("other!u@h", "PRIVMSG", &["me".into(), "hello".into()]);
        assert_eq!(
            model.buffers["other"].messages.back().unwrap(),
            "<other> hello"
        );
        model.disconnected("end");
        assert!(!model.connected);
    }

    #[test]
    fn history_buffers_and_untrusted_display_are_bounded() {
        let mut model = Model::default();
        for _ in 0..600 {
            model.push("#rust", "\x1bhello\x00");
        }
        assert_eq!(model.buffers["#rust"].messages.len(), HISTORY_LIMIT);
        assert_eq!(model.buffers["#rust"].messages[0], "hello");
        for i in 0..100 {
            model.push(&format!("nick{i}"), "message");
        }
        assert_eq!(model.buffers.len(), BUFFER_LIMIT);
    }

    #[test]
    fn server_echo_for_outgoing_private_message_stays_with_peer() {
        let mut model = Model::default();
        model.registered("me".into());
        model.received(
            "me!user@host",
            "PRIVMSG",
            &["friend".into(), "hello".into()],
        );
        assert_eq!(model.buffers["friend"].messages[0], "<me> hello");
        assert!(!model.buffers.contains_key("me"));
    }

    #[test]
    fn remote_labels_and_messages_cannot_change_display_direction() {
        assert_eq!(clean("fr\u{202e}iend\x1b"), "friend");
        let mut model = Model::default();
        model.push("friend", "hello\u{2066}world\x07");
        assert_eq!(model.buffers["friend"].messages[0], "helloworld");
    }

    #[test]
    fn rfc1459_routes_self_channels_and_private_messages_consistently() {
        let mut model = Model::default();
        model.registered("Me[".into());
        model.received("mE{!u@h", "JOIN", &["#Rust".into()]);
        assert_eq!(model.selected, "#Rust");
        model.received("Friend!u@h", "PRIVMSG", &["#rust".into(), "one".into()]);
        model.received("Friend!u@h", "PRIVMSG", &["ME{".into(), "two".into()]);
        model.received("FRIEND!u@h", "PRIVMSG", &["me[".into(), "three".into()]);
        model.received("ME{!u@h", "PRIVMSG", &["fRiEnD".into(), "four".into()]);
        assert_eq!(model.buffers["#Rust"].messages.len(), 2);
        assert_eq!(model.buffers["Friend"].messages.len(), 3);
        assert_eq!(model.buffers.len(), 3);
        model.received("mE{!u@h", "NICK", &["NewNick".into()]);
        assert_eq!(model.nick, "NewNick");
        model.select("#RUST");
        assert_eq!(model.selected, "#Rust");
    }

    #[test]
    fn negotiated_mapping_reconciles_existing_conversations() {
        let mut model = Model::default();
        model.set_case_mapping(CaseMapping::Ascii);
        model.push("Nick[", "one");
        model.push("Nick{", "two");
        assert_eq!(model.buffers.len(), 3);
        model.select("Nick{");
        model.set_case_mapping(CaseMapping::Rfc1459);
        assert_eq!(model.buffers.len(), 2);
        assert_eq!(model.buffers[&model.selected].messages.len(), 2);
    }
}
