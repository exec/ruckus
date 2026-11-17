//! GPUI shell. The core owns networking; the UI polls its bounded event queue.

use std::{sync::Arc, time::Duration};

use gpui::{
    App, Application, Bounds, Context, Entity, FocusHandle, ListAlignment, ListOffset, ListState,
    SharedString, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size, uniform_list,
};
use gpui_component::{
    Disableable, Root,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use ruckus_core::client::{Client, Config, Event};
use ruckus_core::state::State;

use crate::model::{Command, Model, STATUS, clean};

mod view;

#[cfg(all(test, feature = "ui-tests"))]
mod tests;

#[cfg(all(test, feature = "ui-tests"))]
mod interaction_tests;

#[cfg(all(test, feature = "ui-tests"))]
mod polish_tests;

#[derive(Clone, Copy, Debug, PartialEq)]
enum PendingFocus {
    Host,
    Composer,
    Workspace,
}

pub(crate) fn run() {
    Application::new().run(|cx: &mut App| {
        gpui_component::init(cx);
        crate::theme::install(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1240.), px(820.)), cx);
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(900.), px(640.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Ruckus".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| Desktop::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        ) {
            eprintln!("Could not open the Ruckus desktop window: {error}");
            cx.quit();
        }
        cx.activate(true);
    });
}

struct Desktop {
    model: Model,
    client: Option<Client>,
    snapshot: Arc<State>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    nick: Entity<InputState>,
    channel: Entity<InputState>,
    account: Entity<InputState>,
    password: Entity<InputState>,
    composer: Entity<InputState>,
    tls: bool,
    connection_open: bool,
    advanced_open: bool,
    show_members: bool,
    pending_focus: Option<PendingFocus>,
    workspace_focus: FocusHandle,
    connection_feedback: String,
    composer_feedback: String,
    message_list: ListState,
    list_target: String,
    list_revision: u64,
    list_edits: u64,
    list_epoch: u64,
    list_messages: Arc<Vec<String>>,
    follow_messages: bool,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

impl Desktop {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = cx.new(|cx| InputState::new(window, cx).placeholder("irc.example.org"));
        let port = cx.new(|cx| InputState::new(window, cx).default_value("6697"));
        let nick = cx.new(|cx| InputState::new(window, cx).placeholder("Nickname"));
        let channel = cx.new(|cx| InputState::new(window, cx).placeholder("#channel (optional)"));
        let account =
            cx.new(|cx| InputState::new(window, cx).placeholder("SASL account (optional)"));
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("SASL password")
                .masked(true)
        });
        let composer =
            cx.new(|cx| InputState::new(window, cx).placeholder("Write a message or /command…"));
        let subscription = cx.subscribe_in(&composer, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { secondary: false }) {
                this.submit(window, cx);
            }
        });
        let mut subscriptions = vec![subscription];
        for input in [&host, &port, &nick, &channel, &account, &password] {
            subscriptions.push(
                cx.subscribe_in(input, window, |this, _, event, window, cx| {
                    if this.client.is_none()
                        && matches!(event, InputEvent::PressEnter { secondary: false })
                    {
                        this.connect(window, cx);
                    }
                }),
            );
        }
        host.update(cx, |input, cx| input.focus(window, cx));
        let poll = cx.spawn(async |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            model: Model::default(),
            client: None,
            snapshot: Arc::default(),
            host,
            port,
            nick,
            channel,
            account,
            password,
            composer,
            tls: true,
            connection_open: true,
            advanced_open: false,
            show_members: true,
            pending_focus: None,
            workspace_focus: cx.focus_handle(),
            connection_feedback: String::new(),
            composer_feedback: String::new(),
            message_list: ListState::new(0, ListAlignment::Bottom, px(160.)),
            list_target: String::new(),
            list_revision: 0,
            list_edits: 0,
            list_epoch: 0,
            list_messages: Arc::default(),
            follow_messages: true,
            _subscriptions: subscriptions,
            _poll: poll,
        }
    }

    // The destination input must be in the painted tree before native text
    // input is handed over to it. Busy/offline surfaces focus the stable shell.
    fn request_visible_focus(&mut self) {
        self.pending_focus = Some(if self.connection_open && self.client.is_none() {
            PendingFocus::Host
        } else if !self.connection_open && self.model.connected {
            PendingFocus::Composer
        } else {
            PendingFocus::Workspace
        });
    }

    fn show_connection(&mut self) {
        self.connection_open = true;
        self.request_visible_focus();
    }

    fn show_conversation(&mut self) {
        self.connection_open = false;
        self.request_visible_focus();
    }

    fn toggle_connection(&mut self) {
        if self.connection_open {
            self.show_conversation();
        } else {
            self.show_connection();
        }
    }

    fn connection_error(&mut self, message: &str) {
        self.connection_feedback = message.into();
        self.advanced_open = true;
        self.show_connection();
    }

    fn terminal_feedback(&mut self, reason: &str) {
        self.connection_feedback.clear();
        self.composer_feedback.clear();
        if self.connection_open {
            self.connection_feedback = clean(reason);
        } else {
            self.composer_feedback = clean(reason);
        }
        self.request_visible_focus();
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let host = self.host.read(cx).value().trim().to_string();
        let nick = self.nick.read(cx).value().trim().to_string();
        let port = self.port.read(cx).value().parse::<u16>();
        let channel = self.channel.read(cx).value().trim().to_string();
        let sasl = match crate::connection::sasl_credentials(
            self.account.read(cx).value().trim(),
            &self.password.read(cx).value(),
            self.tls,
        ) {
            Ok(sasl) => sasl,
            Err(error) => {
                self.connection_error(error);
                cx.notify();
                return;
            }
        };
        if host.is_empty() || nick.is_empty() || port.is_err() || port == Ok(0) {
            self.connection_error("Enter a server, nickname, and port between 1 and 65535.");
            cx.notify();
            return;
        }
        let config = Config {
            host,
            port: port.unwrap_or(6697),
            tls: self.tls,
            nick: nick.clone(),
            username: nick,
            realname: "Ruckus".into(),
            channels: if channel.is_empty() {
                vec![]
            } else {
                vec![channel]
            },
            sasl,
            ..Default::default()
        };
        match Client::connect(config) {
            Ok(client) => {
                // A new session never inherits channel membership from an old one.
                self.model = Model::default();
                self.snapshot = Arc::default();
                self.model.status = "Connecting…".into();
                self.client = Some(client);
                self.connection_feedback = "Connecting; waiting for IRC registration…".into();
                self.composer_feedback.clear();
                self.request_visible_focus();
                self.password
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
            Err(_) => {
                self.connection_error(
                    "Invalid connection settings. Check server, nickname, and channel.",
                );
            }
        }
        cx.notify();
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let mut terminal_seen = false;
        let was_connected = self.model.connected;
        let previous_selected = self.model.selected.clone();
        let previous_revision = self.model.selected_revision();
        let near_bottom = self.message_list.max_offset_for_scrollbar().height
            + self.message_list.scroll_px_offset_for_scrollbar().y
            <= px(32.);
        // Bound UI work per frame even when a server floods events.
        for _ in 0..128 {
            let Some(event) = self.client.as_ref().and_then(Client::try_recv) else {
                break;
            };
            if !changed && let Some(client) = &self.client {
                self.snapshot = client.snapshot();
                self.model.sync_snapshot(
                    &self.snapshot,
                    client.is_registered(),
                    client.is_closed(),
                );
            }
            changed = true;
            match event {
                Event::Sent { id } => self.model.sent(id),
                Event::Connected { nick } => {
                    self.model.registered(nick);
                    self.connection_feedback.clear();
                }
                Event::Message {
                    message,
                    affected_channels,
                    self_nick,
                } => self
                    .model
                    .receive_message(&message, &affected_channels, &self_nick),
                Event::Disconnected { reason } => {
                    self.model.disconnected(&reason);
                    self.terminal_feedback(&reason);
                    terminal_seen = true;
                }
                Event::Error { message } => self.model.network_notice(message),
                Event::Overflow { dropped } => {
                    self.model.unconfirm_pending();
                    self.model.network_notice(format!(
                        "{dropped} display events dropped; pending send status is unconfirmed."
                    ));
                }
            }
        }
        if terminal_seen {
            // The terminal event is last and cannot be followed by more worker
            // messages. Observing closed + an earlier empty poll is insufficient:
            // publication could race that poll.
            self.client = None;
            self.snapshot = Arc::default();
            self.request_visible_focus();
        } else if let Some(client) = &self.client {
            let previous = (
                self.model.connected,
                self.model.status.clone(),
                self.model.nick.clone(),
            );
            self.snapshot = client.snapshot();
            let closed = client.is_closed();
            self.model
                .sync_snapshot(&self.snapshot, client.is_registered(), closed);
            changed |= previous
                != (
                    self.model.connected,
                    self.model.status.clone(),
                    self.model.nick.clone(),
                );
        }
        if !was_connected && self.model.connected {
            self.show_conversation();
            self.connection_feedback.clear();
            self.composer_feedback.clear();
        } else if was_connected && !self.model.connected {
            self.request_visible_focus();
        }
        if changed {
            if crate::model::should_follow(
                previous_selected != self.model.selected,
                previous_revision != self.model.selected_revision(),
                near_bottom,
            ) {
                self.follow_messages = true;
            }
            cx.notify();
        }
    }

    fn cancel(&mut self) {
        // Both controls are hard cancellation, not a promise to deliver IRC QUIT.
        self.client = None;
        self.snapshot = Arc::default();
        self.model.disconnected(if self.model.connected {
            "Disconnected locally; transport closed immediately."
        } else {
            "Connection attempt cancelled."
        });
        self.connection_feedback.clear();
        self.composer_feedback.clear();
        self.request_visible_focus();
    }

    fn prepare_join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.connected {
            self.show_conversation();
            if self.composer.read(cx).value().is_empty() {
                self.composer
                    .update(cx, |input, cx| input.set_value("/join ", window, cx));
            } else {
                self.composer_feedback =
                    "Your draft is safe. Use /join #channel when you're ready.".into();
            }
            cx.notify();
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.composer.read(cx).value();
        let command = match self.model.command(&input) {
            Ok(command) => command,
            Err(error) => {
                self.composer_feedback = error.into();
                cx.notify();
                return;
            }
        };
        let Some(client) = self.client.as_ref() else {
            return;
        };
        if matches!(command, Command::Message { .. }) && !self.model.can_track_send() {
            self.composer_feedback =
                "Too many pending sends; wait for confirmations before retrying.".into();
            cx.notify();
            return;
        }
        let mut tracked_send = None;
        let result = match &command {
            Command::Join(channel) => client.join(channel),
            Command::Part(channel) => client.part(channel, None),
            Command::Nick(nick) => client.nick(nick),
            Command::Message { target, text } => client.privmsg_tracked(target, text).map(|id| {
                tracked_send = Some(id);
            }),
            Command::Disconnect => {
                client.disconnect();
                Ok(())
            }
        };
        match result {
            Ok(()) => {
                if matches!(command, Command::Disconnect) {
                    self.cancel();
                }
                if let (Some(id), Command::Message { target, text }) = (tracked_send, command) {
                    self.model.queued(id, &target, &text);
                    self.model.select(&target);
                }
                self.composer
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.composer_feedback.clear();
                self.follow_messages = true;
            }
            Err(_) => {
                self.composer_feedback =
                    "Message not queued. Check connection and IRC line length.".into()
            }
        }
        cx.notify();
    }

    fn sync_message_list(&mut self) {
        let buffer = self.model.buffers.get(&self.model.selected);
        let revision = buffer.map(|buffer| buffer.revision).unwrap_or(0);
        let edits = buffer.map(|buffer| buffer.edits).unwrap_or(0);
        let count = buffer.map(|buffer| buffer.messages.len()).unwrap_or(0);
        let changed_target = self.list_target != self.model.selected;
        let epoch = self.model.history_epoch();
        let rebuilt = epoch != self.list_epoch;
        if changed_target || revision < self.list_revision {
            self.message_list.reset(count);
            self.follow_messages = true;
        } else if rebuilt {
            let anchor = self.message_list.logical_scroll_top();
            self.message_list.reset(count);
            self.message_list.scroll_to(anchor);
        } else if revision != self.list_revision {
            let old_count = self.message_list.item_count();
            let added = usize::try_from(revision - self.list_revision)
                .unwrap_or(count)
                .min(count);
            let removed = old_count
                .saturating_add(added)
                .saturating_sub(count)
                .min(old_count);
            if removed > 0 {
                self.message_list.splice(0..removed, 0);
                // A newly exposed first message needs its author heading again.
                if self.message_list.item_count() > 0 {
                    self.message_list.splice(0..1, 1);
                }
            }
            let end = self.message_list.item_count();
            self.message_list.splice(end..end, added);
        }
        if !changed_target && !rebuilt && edits != self.list_edits {
            let anchor = self.message_list.logical_scroll_top();
            self.message_list.reset(count);
            self.message_list.scroll_to(anchor);
        }
        if changed_target || rebuilt || revision != self.list_revision || edits != self.list_edits {
            self.list_messages = Arc::new(
                buffer
                    .into_iter()
                    .flat_map(|buffer| buffer.messages.iter())
                    .map(|entry| entry.text.clone())
                    .collect(),
            );
            self.list_target.clone_from(&self.model.selected);
            self.list_revision = revision;
            self.list_edits = edits;
            self.list_epoch = epoch;
        }
        if self.follow_messages {
            self.message_list.scroll_to(ListOffset {
                item_ix: count,
                offset_in_item: px(0.),
            });
            self.follow_messages = false;
        }
    }
}
