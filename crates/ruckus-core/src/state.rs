//! Deterministic connection state reducer, adapted from Lurk `client/state.go`.
//!
//! State contains raw protocol text: sanitize it at the rendering boundary.
//! No transport, clock, locks, callbacks or terminal dependencies are required.
//!
//! Copyright (c) 2026 exec.
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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::irc::Message;
use crate::isupport::ISupport;

/// Maximum joined channels tracked by a connection.
///
/// 256 accommodates large multi-channel sessions without multiplying ancillary
/// channel state by thousands. Excess new self-JOINs are ignored.
pub const MAX_CHANNELS: usize = 256;
/// Maximum members per channel; excess new identities are ignored.
pub const MAX_MEMBERS: usize = 65_536;
/// Maximum total memberships across all channels, counting shared users once
/// per channel. Existing memberships can still be updated at capacity.
pub const MAX_TOTAL_MEMBERS: usize = 65_536;
/// Maximum combined live-change tombstones and rename deltas per NAMES burst.
///
/// With at most `MAX_CHANNELS` bursts, the connection-wide maximum is 65,536.
/// Overflow abandons the burst until 366, preserving current live membership.
pub const MAX_NAMES_CHANGES: usize = 256;

/// One channel membership, with metadata learned from IRCv3 notifications.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Member {
    /// Display-case nickname.
    pub nick: String,
    /// Membership symbols, in server-advertised privilege order.
    pub prefixes: String,
    /// Username, empty if unknown.
    pub user: String,
    /// Hostname, empty if unknown.
    pub host: String,
    /// Services account, empty if unknown or logged out.
    pub account: String,
    /// Whether away-notify marked this user away; false also means unknown.
    pub away: bool,
    /// Extended-JOIN real name, empty if unknown.
    pub real_name: String,
}

/// A channel topic and its optional server-supplied attribution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Topic {
    /// Raw topic text; empty for no topic or unknown.
    pub text: String,
    /// Nick or mask of its setter, if known.
    pub set_by: String,
    /// Unix seconds from numeric 333, absent if invalid or unknown.
    pub timestamp: Option<i64>,
    /// Unparsed IRCv3 server-time tag for a live TOPIC, if supplied.
    pub server_time: Option<String>,
}

/// A tracked channel. Map keys use the connection's IRC case mapping.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Channel {
    /// Display-case name.
    pub name: String,
    /// Folded nick to membership.
    pub members: BTreeMap<String, Member>,
    /// Current topic.
    pub topic: Topic,
    // Values are display nicks, so an ISUPPORT mapping change can re-key them.
    names_seen: Option<BTreeMap<String, String>>,
    // Live changes during a burst win over later stale 353 chunks.
    names_changed: BTreeSet<String>,
    // Old folded nick -> latest identity; not proof of channel membership.
    names_renamed: BTreeMap<String, Member>,
    // Ignore remaining chunks of an abandoned burst until its closing 366.
    names_aborted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mutation {
    Welcome,
    Support,
    Join,
    Part,
    Kick,
    Quit,
    Nick,
    Names,
    EndNames,
    Mode,
    NoTopic,
    TopicText,
    TopicSetter,
    Topic,
    Account,
    Away,
    Chghost,
}

impl Mutation {
    fn classify(command: &str) -> Option<Self> {
        Some(match command.to_ascii_uppercase().as_str() {
            "001" => Self::Welcome,
            "005" => Self::Support,
            "JOIN" => Self::Join,
            "PART" => Self::Part,
            "KICK" => Self::Kick,
            "QUIT" => Self::Quit,
            "NICK" => Self::Nick,
            "353" => Self::Names,
            "366" => Self::EndNames,
            "MODE" => Self::Mode,
            "331" => Self::NoTopic,
            "332" => Self::TopicText,
            "333" => Self::TopicSetter,
            "TOPIC" => Self::Topic,
            "ACCOUNT" => Self::Account,
            "AWAY" => Self::Away,
            "CHGHOST" => Self::Chghost,
            _ => return None,
        })
    }
}

/// Reusable state for one connection; clone it to obtain an owned snapshot.
///
/// Public fields are intended for reading. Mutating map keys externally can
/// violate lookup invariants. Only complete parsed messages should be applied;
/// the transport must independently bound frame sizes. Membership counts are
/// bounded here, but string sizes rely on that wire-frame bound.
#[derive(Clone, Debug, Default)]
pub struct State {
    /// Current self nickname (updated by 001 and self NICK).
    pub nick: String,
    /// Server-confirmed self username, if known.
    pub user: String,
    /// Server-confirmed self hostname, if known.
    pub host: String,
    /// Confirmed joined channels keyed by folded name. Only self-JOIN creates
    /// entries; unsolicited NAMES and topic replies never fabricate membership.
    pub channels: BTreeMap<String, Channel>,
    /// Server features accumulated from 005.
    pub features: ISupport,
}

impl State {
    /// Create empty state with the proposed nick and default server features.
    pub fn new(nick: impl Into<String>) -> Self {
        Self {
            nick: nick.into(),
            ..Self::default()
        }
    }

    /// Fold an identifier using the active IRC case mapping, not Unicode folding.
    pub fn fold(&self, name: &str) -> String {
        self.features.case_mapping().fold(name)
    }

    /// Look up a channel with IRC case-insensitive matching.
    pub fn channel(&self, name: &str) -> Option<&Channel> {
        self.channels.get(&self.fold(name))
    }

    /// Look up a channel member with IRC case-insensitive matching.
    pub fn member(&self, channel: &str, nick: &str) -> Option<&Member> {
        self.channel(channel)?.members.get(&self.fold(nick))
    }

    /// Return shared channel display names in deterministic folded-key order.
    ///
    /// Call before reducing QUIT/NICK if building an event yourself; `apply`
    /// returns this same pre-mutation capture for those commands.
    pub fn channels_with(&self, nick: &str) -> Vec<String> {
        let key = self.fold(nick);
        self.channels
            .values()
            .filter(|ch| ch.members.contains_key(&key))
            .map(|ch| ch.name.clone())
            .collect()
    }

    /// Number of tracked memberships, counting shared users per channel.
    ///
    /// This sums at most `MAX_CHANNELS` map lengths, not individual members.
    /// Deriving admission budgets from the maps avoids counter drift on removals,
    /// case-map collisions, and externally edited public snapshot fields.
    pub fn total_members(&self) -> usize {
        self.channels.values().map(|ch| ch.members.len()).sum()
    }

    fn remaining_members(&self) -> usize {
        MAX_TOTAL_MEMBERS.saturating_sub(self.total_members())
    }

    fn channel_mut(&mut self, name: &str) -> Option<&mut Channel> {
        let key = self.fold(name);
        self.channels.get_mut(&key)
    }

    fn ensure_channel(&mut self, name: &str) -> Option<&mut Channel> {
        if name.is_empty() {
            return None;
        }
        let key = self.fold(name);
        if !self.channels.contains_key(&key) && self.channels.len() >= MAX_CHANNELS {
            return None;
        }
        Some(self.channels.entry(key).or_insert_with(|| Channel {
            name: name.into(),
            ..Channel::default()
        }))
    }

    /// Apply one message, returning pre-mutation affected channels for QUIT/NICK.
    ///
    /// Unknown commands and incomplete messages are ignored. NAMES chunks update
    /// members immediately; 366 reconciles absent members. Interleaved live JOIN,
    /// PART, KICK, QUIT, NICK and prefix MODE changes take precedence over stale
    /// NAMES chunks. A standalone 366 without a 353 is ignored, since it can also
    /// terminate a query for a nonexistent nick/channel.
    ///
    /// State tracks members, not arbitrary channel modes or history batches.
    pub fn apply(&mut self, message: &Message) -> Vec<String> {
        let Some(mutation) = Mutation::classify(&message.command) else {
            return Vec::new();
        };
        self.apply_mutation(message, mutation)
    }

    /// Apply with copy-on-write only for commands owned by this reducer.
    ///
    /// Stateless replies preserve snapshot identity. Recognized commands may
    /// copy even when incomplete or otherwise ineffective.
    pub fn apply_shared(state: &mut Arc<Self>, message: &Message) -> Vec<String> {
        let Some(mutation) = Mutation::classify(&message.command) else {
            return Vec::new();
        };
        Arc::make_mut(state).apply_mutation(message, mutation)
    }

    fn apply_mutation(&mut self, message: &Message, mutation: Mutation) -> Vec<String> {
        let p = &message.params;
        let (nick, user, host) = split_mask(message.source.as_deref().unwrap_or(""));
        let key = self.fold(nick);
        let is_self = !nick.is_empty() && key == self.fold(&self.nick);
        let mapping = self.features.case_mapping();
        match mutation {
            Mutation::Welcome if !p.is_empty() => self.nick.clone_from(&p[0]),
            Mutation::Support if p.len() >= 2 => self.apply_support(&p[1..p.len() - 1]),
            Mutation::Join if !p.is_empty() && !nick.is_empty() => {
                let mut remaining = self.remaining_members();
                for name in p[0]
                    .split(',')
                    .filter(|name| !name.is_empty() && *name != "0")
                {
                    let channel_key = self.fold(name);
                    let ch = if is_self {
                        self.ensure_channel(name)
                    } else {
                        self.channels.get_mut(&channel_key)
                    };
                    if let Some(ch) = ch {
                        let mut member = ch.members.get(&key).cloned().unwrap_or_default();
                        member.nick = nick.into();
                        member.user = user.into();
                        member.host = host.into();
                        if let Some(account) = p.get(1) {
                            member.account = normalize_account(account).into();
                        }
                        if let Some(real_name) = p.get(2) {
                            member.real_name.clone_from(real_name);
                        }
                        insert_member_with_budget(ch, key.clone(), member, &mut remaining);
                        mark_live(ch, &key);
                    }
                }
                if is_self {
                    self.user = user.into();
                    self.host = host.into();
                }
            }
            Mutation::Part if !p.is_empty() && !nick.is_empty() => {
                for name in p[0].split(',') {
                    let channel_key = self.fold(name);
                    if is_self {
                        self.channels.remove(&channel_key);
                    } else if let Some(ch) = self.channels.get_mut(&channel_key) {
                        remove_member(ch, &key, mapping);
                    }
                }
            }
            Mutation::Kick if p.len() >= 2 => {
                let channel_key = self.fold(&p[0]);
                let self_key = self.fold(&self.nick);
                for target in p[1].split(',') {
                    let target_key = self.fold(target);
                    if target_key == self_key {
                        self.channels.remove(&channel_key);
                    } else if let Some(ch) = self.channels.get_mut(&channel_key) {
                        remove_member(ch, &target_key, mapping);
                    }
                }
            }
            Mutation::Quit if !nick.is_empty() => {
                let affected = self.channels_with(nick);
                if is_self {
                    self.channels.clear();
                } else {
                    for ch in self.channels.values_mut() {
                        remove_member(ch, &key, mapping);
                    }
                }
                return affected;
            }
            Mutation::Nick if !nick.is_empty() && !p.is_empty() && !p[0].is_empty() => {
                let affected = self.channels_with(nick);
                let new_key = self.fold(&p[0]);
                for ch in self.channels.values_mut() {
                    let mut identity = ch.members.get(&key).cloned().unwrap_or_default();
                    for delta in ch.names_renamed.values() {
                        if mapping.fold(&delta.nick) == key {
                            identity = delta.clone();
                            break;
                        }
                    }
                    identity.nick.clone_from(&p[0]);
                    if !user.is_empty() {
                        identity.user = user.into();
                    }
                    if !host.is_empty() {
                        identity.host = host.into();
                    }
                    for delta in ch.names_renamed.values_mut() {
                        if mapping.fold(&delta.nick) == key {
                            *delta = identity.clone();
                        }
                    }
                    if ch.names_seen.is_some() {
                        ch.names_renamed.insert(key.clone(), identity.clone());
                        bound_names_changes(ch);
                        mark_live(ch, &key);
                    }
                    if let Some(mut member) = ch.members.remove(&key) {
                        member.nick.clone_from(&p[0]);
                        member.user = identity.user;
                        member.host = identity.host;
                        insert_member(ch, new_key.clone(), member);
                        mark_live(ch, &key);
                        mark_live(ch, &new_key);
                    }
                }
                if is_self {
                    self.nick.clone_from(&p[0]);
                }
                return affected;
            }
            Mutation::Names if p.len() >= 4 => self.apply_names(&p[2], &p[3]),
            Mutation::EndNames if p.len() >= 2 => {
                let channel_key = self.fold(&p[1]);
                if let Some(ch) = self.channels.get_mut(&channel_key) {
                    if let Some(seen) = ch.names_seen.take() {
                        ch.members.retain(|key, _| seen.contains_key(key));
                    }
                    ch.names_changed.clear();
                    ch.names_renamed.clear();
                    ch.names_aborted = false;
                }
            }
            Mutation::Mode if p.len() >= 2 => self.apply_mode(&p[0], &p[1], &p[2..]),
            Mutation::NoTopic if p.len() >= 2 => {
                if let Some(ch) = self.channel_mut(&p[1]) {
                    ch.topic = Topic::default();
                }
            }
            Mutation::TopicText if p.len() >= 3 => {
                if let Some(ch) = self.channel_mut(&p[1]) {
                    ch.topic = Topic {
                        text: p[2].clone(),
                        ..Topic::default()
                    };
                }
            }
            Mutation::TopicSetter if p.len() >= 4 => {
                if let Some(ch) = self.channel_mut(&p[1]) {
                    ch.topic.set_by.clone_from(&p[2]);
                    ch.topic.timestamp = p[3].parse().ok();
                }
            }
            Mutation::Topic if p.len() >= 2 => {
                if let Some(ch) = self.channel_mut(&p[0]) {
                    ch.topic = Topic {
                        text: p[1].clone(),
                        set_by: message.source.clone().unwrap_or_default(),
                        server_time: message.tags.get("time").cloned(),
                        timestamp: None,
                    };
                }
            }
            Mutation::Account | Mutation::Away | Mutation::Chghost if !nick.is_empty() => {
                let mapping = self.features.case_mapping();
                for ch in self.channels.values_mut() {
                    for member in ch.members.get_mut(&key).into_iter().chain(
                        ch.names_renamed
                            .values_mut()
                            .filter(|delta| mapping.fold(&delta.nick) == key),
                    ) {
                        match mutation {
                            Mutation::Account if !p.is_empty() => {
                                member.account = normalize_account(&p[0]).into()
                            }
                            Mutation::Away => member.away = !p.is_empty(),
                            Mutation::Chghost if p.len() >= 2 => {
                                member.user.clone_from(&p[0]);
                                member.host.clone_from(&p[1]);
                            }
                            _ => {}
                        }
                    }
                }
                if is_self && mutation == Mutation::Chghost && p.len() >= 2 {
                    self.user.clone_from(&p[0]);
                    self.host.clone_from(&p[1]);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn apply_names(&mut self, channel: &str, names: &str) {
        let mapping = self.features.case_mapping();
        let symbols = self.features.prefix().1.to_string();
        let mut remaining = self.remaining_members();
        let Some(ch) = self.channel_mut(channel) else {
            return;
        };
        if ch.names_aborted {
            return;
        }
        ch.names_seen.get_or_insert_with(BTreeMap::new);
        for entry in names.split_ascii_whitespace() {
            let mask = entry.trim_start_matches(|c| symbols.contains(c));
            let raw_prefixes = &entry[..entry.len() - mask.len()];
            let (nick, user, host) = split_mask(mask);
            if nick.is_empty() {
                continue;
            }
            let key = mapping.fold(nick);
            if let Some(mut member) = ch.names_renamed.get(&key).cloned() {
                let new_key = mapping.fold(&member.nick);
                if !ch.members.contains_key(&new_key) {
                    member.prefixes = symbols
                        .chars()
                        .filter(|c| raw_prefixes.contains(*c))
                        .collect();
                    if member.user.is_empty() {
                        member.user = user.into();
                    }
                    if member.host.is_empty() {
                        member.host = host.into();
                    }
                    insert_member_with_budget(ch, new_key.clone(), member, &mut remaining);
                }
                mark_live(ch, &new_key);
                if ch.names_aborted {
                    break;
                }
                continue;
            }
            if ch.names_changed.contains(&key) {
                continue;
            }
            let mut member = ch.members.get(&key).cloned().unwrap_or_default();
            member.nick = nick.into();
            // An unprefixed NAMES entry explicitly clears stale privileges.
            member.prefixes = symbols
                .chars()
                .filter(|c| raw_prefixes.contains(*c))
                .collect();
            if !user.is_empty() {
                member.user = user.into();
            }
            if !host.is_empty() {
                member.host = host.into();
            }
            insert_member_with_budget(ch, key.clone(), member, &mut remaining);
            if ch.members.contains_key(&key)
                && let Some(seen) = &mut ch.names_seen
            {
                seen.insert(key, nick.into());
            }
        }
    }

    fn apply_support(&mut self, tokens: &[String]) {
        let old_mapping = self.features.case_mapping();
        let tokens: Vec<&str> = tokens.iter().map(String::as_str).collect();
        self.features = self.features.merge(&tokens);
        let mapping = self.features.case_mapping();
        if old_mapping == mapping {
            return;
        }
        let mut channels: BTreeMap<String, Channel> = BTreeMap::new();
        for mut ch in std::mem::take(&mut self.channels).into_values() {
            ch.members = std::mem::take(&mut ch.members)
                .into_values()
                .map(|member| (mapping.fold(&member.nick), member))
                .collect();
            // End the in-flight reconciliation rather than interpret tombstones
            // folded under a different mapping. Existing memberships survive.
            ch.names_aborted |= ch.names_seen.is_some();
            ch.names_seen = None;
            ch.names_changed.clear();
            ch.names_renamed.clear();
            let key = mapping.fold(&ch.name);
            if let Some(existing) = channels.get_mut(&key) {
                existing.names_aborted |= ch.names_aborted;
                for (key, member) in ch.members {
                    insert_member(existing, key, member);
                }
            } else {
                channels.insert(key, ch);
            }
        }
        self.channels = channels;
    }

    fn apply_mode(&mut self, channel: &str, modes: &str, args: &[String]) {
        let channel_key = self.fold(channel);
        let mapping = self.features.case_mapping();
        let symbols = self.features.prefix().1.to_string();
        let chan_modes = self.features.chan_modes();
        let mut changes = Vec::new();
        let mut adding = true;
        let mut args = args.iter();
        for mode in modes.chars() {
            match mode {
                '+' => {
                    adding = true;
                    continue;
                }
                '-' => {
                    adding = false;
                    continue;
                }
                _ => {}
            }
            if let Some(symbol) = self.features.prefix_symbol_for_mode(mode) {
                let Some(nick) = args.next() else { break };
                changes.push((mapping.fold(nick), symbol, adding));
            } else {
                match chan_modes.takes_parameter(mode, adding) {
                    Some(true) if args.next().is_none() => break,
                    Some(_) => {}
                    // Unknown argument arity makes later nick arguments
                    // ambiguous. Do not assign privileges to the wrong user.
                    None => break,
                }
            }
        }
        let Some(ch) = self.channels.get_mut(&channel_key) else {
            return;
        };
        for (key, symbol, adding) in changes {
            if let Some(member) = ch.members.get_mut(&key) {
                member.prefixes = symbols
                    .chars()
                    .filter(|c| {
                        if *c == symbol {
                            adding
                        } else {
                            member.prefixes.contains(*c)
                        }
                    })
                    .collect();
                mark_live(ch, &key);
            }
        }
    }
}

/// Split a source or userhost-in-names mask without allocating.
///
/// Missing user/host components are returned as empty strings.
pub fn split_mask(mask: &str) -> (&str, &str, &str) {
    let (left, host) = mask.split_once('@').unwrap_or((mask, ""));
    let (nick, user) = left.split_once('!').unwrap_or((left, ""));
    (nick, user, host)
}

fn normalize_account(account: &str) -> &str {
    if account == "*" { "" } else { account }
}

fn insert_member(ch: &mut Channel, key: String, member: Member) {
    if ch.members.contains_key(&key) || ch.members.len() < MAX_MEMBERS {
        ch.members.insert(key, member);
    }
}

// Only JOIN/NAMES introduce new memberships. Their shared per-message budget
// is derived once from the bounded channel map, then consumed across all entries.
// NICK and CASEMAPPING re-key existing members and cannot increase the total.
fn insert_member_with_budget(ch: &mut Channel, key: String, member: Member, remaining: &mut usize) {
    if ch.members.contains_key(&key) {
        ch.members.insert(key, member);
    } else if *remaining > 0 && ch.members.len() < MAX_MEMBERS {
        ch.members.insert(key, member);
        *remaining -= 1;
    }
}

fn mark_live(ch: &mut Channel, key: &str) {
    if ch.names_seen.is_some() {
        ch.names_changed.insert(key.into());
        bound_names_changes(ch);
    }
    if let Some(seen) = &mut ch.names_seen {
        if let Some(member) = ch.members.get(key) {
            seen.insert(key.into(), member.nick.clone());
        } else {
            seen.remove(key);
        }
    }
}

fn bound_names_changes(ch: &mut Channel) {
    if ch.names_changed.len() + ch.names_renamed.len() > MAX_NAMES_CHANGES {
        ch.names_seen = None;
        ch.names_changed.clear();
        ch.names_renamed.clear();
        ch.names_aborted = true;
    }
}

fn remove_member(ch: &mut Channel, key: &str, mapping: crate::isupport::CaseMapping) {
    ch.members.remove(key);
    ch.names_renamed
        .retain(|_, member| mapping.fold(&member.nick) != key);
    mark_live(ch, key);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(source: &str, command: &str, params: &[&str]) -> Message {
        Message {
            source: (!source.is_empty()).then(|| source.into()),
            command: command.into(),
            params: params.iter().map(|s| (*s).into()).collect(),
            tags: BTreeMap::new(),
        }
    }

    fn apply(state: &mut State, source: &str, command: &str, params: &[&str]) {
        state.apply(&message(source, command, params));
    }

    fn joined() -> State {
        let mut state = State::new("Me");
        apply(&mut state, "Me!self@host", "JOIN", &["#Test"]);
        state
    }

    #[test]
    fn membership_and_cross_channel_event_capture() {
        let mut state = joined();
        apply(&mut state, "Me!self@host", "JOIN", &["#other"]);
        for channel in ["#test", "#other"] {
            apply(
                &mut state,
                "Nick!user@host",
                "JOIN",
                &[channel, "account", "Real Name"],
            );
        }
        assert_eq!(
            state.member("#TEST", "NICK").unwrap().real_name,
            "Real Name"
        );
        let affected = state.apply(&message("Nick!user@host", "NICK", &["Renamed"]));
        assert_eq!(affected, ["#other", "#Test"]);
        assert!(state.member("#test", "nick").is_none());
        assert_eq!(state.member("#test", "renamed").unwrap().account, "account");
        let affected = state.apply(&message("Renamed", "QUIT", &["bye"]));
        assert_eq!(affected.len(), 2);
        assert!(state.member("#test", "renamed").is_none());
        apply(&mut state, "me", "NICK", &["NewMe"]);
        assert_eq!(state.nick, "NewMe");
        apply(&mut state, "op", "KICK", &["#test", "NEWME"]);
        assert!(state.channel("#test").is_none());
        apply(&mut state, "newme", "PART", &["#OTHER"]);
        assert!(state.channels.is_empty());
    }

    #[test]
    fn names_reconcile_preserves_metadata_and_live_changes() {
        let mut state = joined();
        apply(&mut state, "Old", "JOIN", &["#test"]);
        apply(&mut state, "Keep", "JOIN", &["#test", "account", "real"]);
        apply(
            &mut state,
            "",
            "353",
            &["Me", "=", "#test", "@+Keep!user@host Gone"],
        );
        apply(&mut state, "Live", "JOIN", &["#test"]);
        apply(&mut state, "Gone", "PART", &["#test"]);
        apply(&mut state, "Keep", "NICK", &["New"]);
        apply(&mut state, "", "353", &["Me", "=", "#test", "Gone Keep Me"]);
        apply(&mut state, "", "366", &["Me", "#test", "end"]);
        assert!(state.member("#test", "old").is_none());
        assert!(state.member("#test", "gone").is_none());
        assert!(state.member("#test", "keep").is_none());
        assert!(state.member("#test", "live").is_some());
        let member = state.member("#test", "new").unwrap();
        assert_eq!(member.prefixes, "@+");
        assert_eq!(member.account, "account");
        assert_eq!(member.user, "user");
        assert_eq!(member.host, "host");
        // A later authoritative burst can remove stale privileges.
        apply(&mut state, "", "353", &["Me", "=", "#test", "New"]);
        apply(&mut state, "", "366", &["Me", "#test", "end"]);
        assert_eq!(state.member("#test", "new").unwrap().prefixes, "");
        assert_eq!(state.channel("#test").unwrap().members.len(), 1);
    }

    #[test]
    fn metadata_and_topics_are_deterministic() {
        let mut state = joined();
        apply(&mut state, "Nick!u@h", "JOIN", &["#test", "*", "real"]);
        apply(&mut state, "Nick", "ACCOUNT", &["logged-in"]);
        apply(&mut state, "Nick", "AWAY", &["gone"]);
        apply(&mut state, "Nick", "CHGHOST", &["newuser", "newhost"]);
        let member = state.member("#test", "nick").unwrap();
        assert_eq!(member.account, "logged-in");
        assert!(member.away);
        assert_eq!((&*member.user, &*member.host), ("newuser", "newhost"));
        apply(&mut state, "Nick", "AWAY", &[]);
        apply(&mut state, "Nick", "ACCOUNT", &["*"]);
        assert!(!state.member("#test", "nick").unwrap().away);
        assert_eq!(state.member("#test", "nick").unwrap().account, "");
        apply(&mut state, "", "332", &["Me", "#test", "topic"]);
        apply(&mut state, "", "333", &["Me", "#test", "setter", "123"]);
        assert_eq!(state.channel("#test").unwrap().topic.timestamp, Some(123));
        let mut topic = message("Nick!u@h", "TOPIC", &["#test", "new topic"]);
        topic
            .tags
            .insert("time".into(), "2026-01-01T00:00:00Z".into());
        state.apply(&topic);
        let topic = &state.channel("#test").unwrap().topic;
        assert_eq!(topic.set_by, "Nick!u@h");
        assert_eq!(topic.timestamp, None);
        assert_eq!(topic.server_time.as_deref(), Some("2026-01-01T00:00:00Z"));
        apply(&mut state, "", "331", &["Me", "#test", "none"]);
        assert_eq!(state.channel("#test").unwrap().topic, Topic::default());
    }

    #[test]
    fn features_rekey_and_mode_arguments_align() {
        let mut state = joined();
        apply(
            &mut state,
            "",
            "005",
            &[
                "Me",
                "CASEMAPPING=ascii",
                "PREFIX=(qaohv)~&@%+",
                "CHANMODES=beI,k,l,imnpst",
                "supported",
            ],
        );
        apply(&mut state, "Nick[", "JOIN", &["#test"]);
        assert!(state.member("#test", "nick{").is_none());
        apply(
            &mut state,
            "op",
            "MODE",
            &["#test", "+bko", "*!*@bad", "secret", "Nick["],
        );
        assert_eq!(state.member("#test", "nick[").unwrap().prefixes, "@");
        apply(
            &mut state,
            "op",
            "MODE",
            &["#test", "+v-q", "Nick[", "Nick["],
        );
        assert_eq!(state.member("#test", "nick[").unwrap().prefixes, "@+");
        apply(
            &mut state,
            "",
            "005",
            &["Me", "CASEMAPPING=rfc1459", "supported"],
        );
        assert!(state.member("#TEST", "nick{").is_some());
        apply(
            &mut state,
            "op",
            "MODE",
            &["#test", "-lov", "Nick{", "Nick{"],
        );
        assert_eq!(state.member("#test", "nick[").unwrap().prefixes, "");
    }

    #[test]
    fn malformed_messages_and_unrelated_joins_do_not_create_state() {
        let mut state = State::new("Me");
        for command in [
            "001", "005", "JOIN", "PART", "KICK", "NICK", "MODE", "353", "366", "TOPIC", "331",
            "332", "333", "ACCOUNT", "CHGHOST",
        ] {
            apply(&mut state, "", command, &[]);
        }
        apply(&mut state, "Other", "JOIN", &["#unrelated"]);
        assert!(state.channels.is_empty());
        apply(&mut state, "", "001", &["Confirmed", "welcome"]);
        assert_eq!(state.nick, "Confirmed");
        assert_eq!(split_mask("nick!u@h"), ("nick", "u", "h"));
    }

    #[test]
    fn admission_limits_bound_members_and_names_tracking() {
        let mut channel = Channel::default();
        for i in 0..=MAX_MEMBERS {
            let nick = format!("n{i}");
            insert_member(
                &mut channel,
                nick.clone(),
                Member {
                    nick,
                    ..Member::default()
                },
            );
        }
        assert_eq!(channel.members.len(), MAX_MEMBERS);
        channel.names_seen = Some(BTreeMap::new());
        for i in 0..=MAX_NAMES_CHANGES {
            mark_live(&mut channel, &format!("absent{i}"));
        }
        assert!(channel.names_changed.len() <= MAX_NAMES_CHANGES);
        assert!(channel.names_seen.is_none());
        assert!(channel.names_aborted);
    }

    #[test]
    fn names_respects_live_prefix_changes_and_ignores_unmatched_end() {
        let mut state = joined();
        apply(&mut state, "", "366", &["Me", "#test", "end"]);
        assert!(state.member("#test", "me").is_some());
        apply(&mut state, "", "353", &["Me", "=", "#test", "@Nick Me"]);
        apply(&mut state, "op", "MODE", &["#test", "-o+v", "Nick", "Nick"]);
        apply(&mut state, "", "353", &["Me", "=", "#test", "@Nick"]);
        apply(&mut state, "", "366", &["Me", "#test", "end"]);
        assert_eq!(state.member("#test", "nick").unwrap().prefixes, "+");
        // An explicit empty 353 is an authoritative empty burst.
        apply(&mut state, "", "353", &["Me", "=", "#test", ""]);
        apply(&mut state, "", "366", &["Me", "#test", "end"]);
        assert!(state.channel("#test").unwrap().members.is_empty());
    }

    #[test]
    fn ambiguous_mode_arity_does_not_grant_wrong_member_privileges() {
        let mut state = joined();
        apply(&mut state, "Nick", "JOIN", &["#test"]);
        apply(&mut state, "op", "MODE", &["#test", "+zo", "Nick", "Me"]);
        assert_eq!(state.member("#test", "me").unwrap().prefixes, "");
        assert_eq!(state.member("#test", "nick").unwrap().prefixes, "");
    }

    #[test]
    fn admission_limits_channels_without_blocking_updates() {
        let mut state = State::new("Me");
        for i in 0..=MAX_CHANNELS {
            apply(&mut state, "Me", "JOIN", &[&format!("#c{i}")]);
        }
        assert_eq!(state.channels.len(), MAX_CHANNELS);
        apply(&mut state, "Me", "TOPIC", &["#c0", "updated"]);
        assert_eq!(state.channel("#c0").unwrap().topic.text, "updated");
        apply(&mut state, "Me", "PART", &["#c0"]);
        apply(&mut state, "Me", "JOIN", &["#new"]);
        assert!(state.channel("#new").is_some());
        assert_eq!(state.channels.len(), MAX_CHANNELS);
    }

    #[test]
    fn unsolicited_names_and_topics_cannot_exhaust_joined_channel_slots() {
        let mut state = State::new("Me");
        for i in 0..MAX_CHANNELS * 2 {
            let channel = format!("#unsolicited{i}");
            apply(&mut state, "", "331", &["Me", &channel, "none"]);
            apply(&mut state, "", "332", &["Me", &channel, "topic"]);
            apply(&mut state, "", "333", &["Me", &channel, "setter", "1"]);
            apply(&mut state, "Other", "TOPIC", &[&channel, "topic"]);
            apply(&mut state, "", "353", &["Me", "=", &channel, "@Other"]);
        }
        assert!(state.channels.is_empty());
        apply(&mut state, "Me", "JOIN", &["#real"]);
        apply(&mut state, "", "353", &["Me", "=", "#real", "Me Other"]);
        apply(&mut state, "", "332", &["Me", "#real", "real topic"]);
        assert_eq!(state.channel("#real").unwrap().members.len(), 2);
        assert_eq!(state.channel("#real").unwrap().topic.text, "real topic");
        apply(&mut state, "Me", "PART", &["#real"]);
        apply(&mut state, "", "353", &["Me", "=", "#real", "@Other"]);
        apply(&mut state, "Other", "TOPIC", &["#real", "late topic"]);
        assert!(state.channels.is_empty());
    }

    #[test]
    fn total_memberships_are_bounded_with_updates_and_reclaimed_capacity() {
        let mut state = joined();
        apply(&mut state, "Me", "JOIN", &["#other"]);
        // Use realistic-sized chunks; each chunk shares one admission budget.
        for start in (0..MAX_TOTAL_MEMBERS).step_by(64) {
            let names = (start..start + 64)
                .map(|i| format!("n{i}"))
                .collect::<Vec<_>>()
                .join(" ");
            let channel = if start < MAX_TOTAL_MEMBERS / 2 {
                "#test"
            } else {
                "#other"
            };
            apply(&mut state, "", "353", &["Me", "=", channel, &names]);
        }
        assert_eq!(state.total_members(), MAX_TOTAL_MEMBERS);
        apply(&mut state, "Overflow", "JOIN", &["#test,#other"]);
        assert!(state.member("#test", "Overflow").is_none());
        assert!(state.member("#other", "Overflow").is_none());
        // Updates remain possible at capacity, both from JOIN and NAMES.
        apply(&mut state, "n0", "JOIN", &["#test", "account"]);
        apply(&mut state, "", "353", &["Me", "=", "#other", "@n32768"]);
        assert_eq!(state.member("#test", "n0").unwrap().account, "account");
        assert_eq!(state.member("#other", "n32768").unwrap().prefixes, "@");
        for (source, command, params) in [
            ("n1", "PART", vec!["#test"]),
            ("op", "KICK", vec!["#test", "n2"]),
            ("n3", "QUIT", vec!["gone"]),
        ] {
            apply(&mut state, source, command, &params);
        }
        assert_eq!(state.total_members(), MAX_TOTAL_MEMBERS - 3);
        apply(&mut state, "New", "JOIN", &["#test,#other"]);
        apply(&mut state, "Another", "JOIN", &["#test,#other"]);
        assert_eq!(state.total_members(), MAX_TOTAL_MEMBERS);
        assert!(state.member("#test", "Another").is_some());
        assert!(state.member("#other", "Another").is_none());
        // Rekey and rename cannot inflate aggregate counts.
        apply(&mut state, "New", "NICK", &["Renamed"]);
        apply(
            &mut state,
            "",
            "005",
            &["Me", "CASEMAPPING=ascii", "supported"],
        );
        assert_eq!(state.total_members(), MAX_TOTAL_MEMBERS);
        apply(&mut state, "Me", "PART", &["#other"]);
        let after_part = state.total_members();
        assert!(after_part < MAX_TOTAL_MEMBERS);
        apply(&mut state, "Reclaimed", "JOIN", &["#test"]);
        assert_eq!(state.total_members(), after_part + 1);
    }

    #[test]
    fn live_names_tombstones_have_a_connection_wide_bound() {
        let mut state = State::new("Me");
        for i in 0..MAX_CHANNELS {
            let channel = format!("#c{i}");
            apply(&mut state, "Me", "JOIN", &[&channel]);
            apply(&mut state, "", "353", &["Me", "=", &channel, "Me"]);
            for j in 0..MAX_NAMES_CHANGES {
                apply(&mut state, &format!("absent{j}"), "PART", &[&channel]);
            }
        }
        let tombstones: usize = state
            .channels
            .values()
            .map(|ch| ch.names_changed.len())
            .sum();
        assert_eq!(tombstones, MAX_CHANNELS * MAX_NAMES_CHANGES);
        assert_eq!(tombstones, 65_536);
        for i in 0..MAX_CHANNELS {
            let channel = format!("#c{i}");
            // Duplicate changes consume no new budget.
            apply(&mut state, "absent0", "PART", &[&channel]);
            assert!(!state.channel(&channel).unwrap().names_aborted);
            apply(&mut state, "one-more", "PART", &[&channel]);
            assert!(state.channel(&channel).unwrap().names_aborted);
            apply(&mut state, "", "353", &["Me", "=", &channel, "Ghost"]);
            assert!(state.member(&channel, "Ghost").is_none());
            apply(&mut state, "", "366", &["Me", &channel, "end"]);
            assert!(state.member(&channel, "Me").is_some());
        }
        assert!(
            state
                .channels
                .values()
                .all(|ch| ch.names_changed.is_empty())
        );
        apply(&mut state, "", "353", &["Me", "=", "#c0", "Fresh"]);
        assert!(state.member("#c0", "Fresh").is_some());
    }

    #[test]
    fn lowercase_metadata_commands_use_the_same_reducer_rules() {
        let mut state = joined();
        apply(&mut state, "Me", "account", &["logged-in"]);
        apply(&mut state, "Me", "away", &["gone"]);
        apply(&mut state, "Me", "chghost", &["newuser", "newhost"]);
        let member = state.member("#test", "Me").unwrap();
        assert_eq!(member.account, "logged-in");
        assert!(member.away);
        assert_eq!((&*member.user, &*member.host), ("newuser", "newhost"));
        assert_eq!((&*state.user, &*state.host), ("newuser", "newhost"));
    }

    #[test]
    fn shared_dispatch_preserves_stateless_snapshot_identity() {
        let mut state = Arc::new(joined());
        let snapshot = Arc::clone(&state);
        for command in [
            "322",
            "311",
            "312",
            "319",
            "318",
            "unknown",
            "PRIVMSG",
            "NOTICE",
            "TAGMSG",
            "PING",
            "PONG",
            "CAP",
            "AUTHENTICATE",
            "BATCH",
        ] {
            assert!(State::apply_shared(&mut state, &message("", command, &[])).is_empty());
            assert!(Arc::ptr_eq(&state, &snapshot), "{command}");
        }
        State::apply_shared(&mut state, &message("New!u@h", "join", &["#test"]));
        assert!(!Arc::ptr_eq(&state, &snapshot));
        assert!(snapshot.member("#test", "New").is_none());
        assert!(state.member("#test", "New").is_some());
    }

    #[test]
    fn fragmented_names_resolves_unknown_renames_only_when_listed() {
        for listed in [false, true] {
            let mut state = joined();
            apply(&mut state, "", "353", &["Me", "=", "#test", "Me First"]);
            apply(&mut state, "Late!live@host", "NICK", &["Renamed"]);
            assert!(state.member("#test", "Renamed").is_none());
            if listed {
                apply(
                    &mut state,
                    "",
                    "353",
                    &["Me", "=", "#test", "@Late!stale@old"],
                );
            }
            apply(&mut state, "", "366", &["Me", "#test", "end"]);
            assert!(state.member("#test", "Late").is_none());
            assert_eq!(state.member("#test", "Renamed").is_some(), listed);
            if listed {
                let member = state.member("#test", "Renamed").unwrap();
                assert_eq!(member.nick, "Renamed");
                assert_eq!(member.prefixes, "@");
                assert_eq!((&*member.user, &*member.host), ("live", "host"));
            }
        }
    }

    #[test]
    fn fragmented_names_rename_chains_keep_latest_metadata() {
        let mut state = joined();
        apply(&mut state, "", "353", &["Me", "=", "#test", "Me First"]);
        apply(&mut state, "Late!u@h", "NICK", &["Middle"]);
        apply(&mut state, "Middle", "ACCOUNT", &["account"]);
        apply(&mut state, "Middle", "AWAY", &["gone"]);
        apply(&mut state, "Middle", "CHGHOST", &["current", "host"]);
        apply(&mut state, "Middle", "NICK", &["Final"]);
        apply(
            &mut state,
            "",
            "353",
            &["Me", "=", "#test", "+lATE!old@stale Middle"],
        );
        apply(&mut state, "", "366", &["Me", "#test", "end"]);
        assert!(state.member("#test", "Late").is_none());
        assert!(state.member("#test", "Middle").is_none());
        let member = state.member("#test", "Final").unwrap();
        assert_eq!(member.nick, "Final");
        assert_eq!(member.account, "account");
        assert!(member.away);
        assert_eq!(member.prefixes, "+");
        assert_eq!((&*member.user, &*member.host), ("current", "host"));
    }

    #[test]
    fn fragmented_names_renames_use_server_casemapping() {
        let mut state = joined();
        apply(&mut state, "", "353", &["Me", "=", "#test", "Me"]);
        apply(&mut state, "Late[!u@h", "NICK", &["Renamed"]);
        apply(&mut state, "", "353", &["Me", "=", "#test", "lATE{"]);
        apply(&mut state, "", "366", &["Me", "#test", "end"]);
        assert!(state.member("#test", "Late[").is_none());
        assert_eq!(state.member("#test", "Renamed").unwrap().user, "u");
    }

    #[test]
    fn fragmented_names_removals_cancel_pending_rename_membership() {
        for (source, command, params) in [
            ("Renamed", "PART", vec!["#test"]),
            ("Renamed", "QUIT", vec!["bye"]),
            ("op", "KICK", vec!["#test", "Renamed"]),
        ] {
            let mut state = joined();
            apply(&mut state, "", "353", &["Me", "=", "#test", "Me"]);
            apply(&mut state, "Late", "NICK", &["Renamed"]);
            apply(&mut state, source, command, &params);
            apply(&mut state, "", "353", &["Me", "=", "#test", "Late Renamed"]);
            apply(&mut state, "", "366", &["Me", "#test", "end"]);
            assert!(state.member("#test", "Late").is_none(), "{command}");
            assert!(state.member("#test", "Renamed").is_none(), "{command}");
        }
    }

    #[test]
    fn rename_deltas_share_change_bound_and_mapping_changes_abandon_burst() {
        let mut state = joined();
        apply(&mut state, "", "353", &["Me", "=", "#test", "Me"]);
        for i in 0..=MAX_NAMES_CHANGES {
            apply(
                &mut state,
                &format!("old{i}"),
                "NICK",
                &[&format!("new{i}")],
            );
            let ch = state.channel("#test").unwrap();
            assert!(ch.names_changed.len() + ch.names_renamed.len() <= MAX_NAMES_CHANGES);
        }
        let ch = state.channel("#test").unwrap();
        assert!(ch.names_aborted);
        assert!(ch.names_renamed.is_empty());
        apply(&mut state, "", "353", &["Me", "=", "#test", "old0 Ghost"]);
        apply(&mut state, "", "366", &["Me", "#test", "end"]);
        assert_eq!(state.channel("#test").unwrap().members.len(), 1);
        apply(&mut state, "", "353", &["Me", "=", "#test", "Me"]);
        apply(&mut state, "Late[", "NICK", &["New"]);
        apply(
            &mut state,
            "",
            "005",
            &["Me", "CASEMAPPING=ascii", "supported"],
        );
        assert!(state.channel("#test").unwrap().names_aborted);
        assert!(state.channel("#test").unwrap().names_renamed.is_empty());
        apply(&mut state, "", "353", &["Me", "=", "#test", "Late["]);
        assert!(state.member("#test", "New").is_none());
    }

    #[test]
    fn pending_renames_cannot_bypass_membership_admission() {
        let mut state = joined();
        apply(&mut state, "", "353", &["Me", "=", "#test", "Me"]);
        apply(&mut state, "Late", "NICK", &["Renamed"]);
        let ch = state.channel_mut("#test").unwrap();
        for i in 1..MAX_TOTAL_MEMBERS {
            let nick = format!("member{i}");
            ch.members.insert(
                nick.clone(),
                Member {
                    nick,
                    ..Member::default()
                },
            );
        }
        apply(&mut state, "", "353", &["Me", "=", "#test", "Late"]);
        assert_eq!(state.total_members(), MAX_TOTAL_MEMBERS);
        assert!(state.member("#test", "Renamed").is_none());
        apply(&mut state, "member1", "PART", &["#test"]);
        apply(&mut state, "", "353", &["Me", "=", "#test", "Late"]);
        assert_eq!(state.total_members(), MAX_TOTAL_MEMBERS);
        assert!(state.member("#test", "Renamed").is_some());
    }
}
