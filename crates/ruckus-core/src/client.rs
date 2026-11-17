//! Bounded, background IRC client shared by desktop and other frontends.
//!
//! Registration and event semantics are adapted from MIT-licensed Lurk,
//! copyright (c) 2026 exec. Runtime ownership is deliberately independent of UI.

use crate::{cap, irc::Message, state::State, transport::Transport};
use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

const EVENT_CAPACITY: usize = 512;
const COMMAND_CAPACITY: usize = 64;
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(60);

/// An identifier for one logical send, scoped to one [`Client`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SendId(
    /// Monotonically allocated connection-local value.
    pub u64,
);

struct Outbound {
    message: Message,
    completed_send: Option<SendId>,
}

impl From<Message> for Outbound {
    fn from(message: Message) -> Self {
        Self {
            message,
            completed_send: None,
        }
    }
}

/// Credentials for SASL PLAIN. Debug output never includes the password.
#[derive(Clone)]
pub struct SaslCredentials {
    /// Account name.
    pub username: String,
    /// Account password; only transmitted over verified TLS.
    pub password: String,
}

impl fmt::Debug for SaslCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SaslCredentials")
            .field("username", &"[redacted]")
            .field("password", &"[redacted]")
            .finish()
    }
}

/// Connection and registration settings. TLS is enabled by default.
#[derive(Clone)]
pub struct Config {
    /// DNS hostname or IP address, without a scheme or port.
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// Use verified TLS. Credentials are refused when this is false.
    pub tls: bool,
    /// Initial nickname.
    pub nick: String,
    /// IRC username.
    pub username: String,
    /// IRC real name.
    pub realname: String,
    /// Channels to join after successful registration.
    pub channels: Vec<String>,
    /// Optional server password.
    pub server_password: Option<String>,
    /// Optional required SASL PLAIN authentication.
    pub sasl: Option<SaslCredentials>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 6697,
            tls: true,
            nick: "ruckus".into(),
            username: "ruckus".into(),
            realname: "Ruckus".into(),
            channels: Vec::new(),
            server_password: None,
            sasl: None,
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("nick", &self.nick)
            .field(
                "server_password",
                &self.server_password.as_ref().map(|_| "[redacted]"),
            )
            .field("sasl", &self.sasl)
            .finish_non_exhaustive()
    }
}

/// An observable event. Raw remote text must be sanitized before display.
#[derive(Clone, Debug)]
pub enum Event {
    /// All wire fragments of a tracked send were written and flushed.
    ///
    /// This is not a server acknowledgement or a recipient delivery receipt.
    Sent {
        /// Identifier returned by [`Client::privmsg_tracked`].
        id: SendId,
    },
    /// Registration completed; joins are now allowed.
    Connected {
        /// Nickname confirmed by the server.
        nick: String,
    },
    /// A parsed server message, with tags and pre-mutation routing preserved.
    Message {
        /// Server command, normalized to ASCII uppercase.
        message: Message,
        /// Shared channel names captured before a QUIT or NICK mutation.
        affected_channels: Vec<String>,
        /// Self nickname when this message arrived, before applying its changes.
        /// Use this for FIFO event routing when a snapshot is already newer.
        self_nick: String,
    },
    /// The connection ended. No automatic reconnect is attempted.
    Disconnected {
        /// Non-secret diagnostic.
        reason: String,
    },
    /// An asynchronous command failed.
    Error {
        /// Non-secret diagnostic.
        message: String,
    },
    /// The bounded stream discarded older events.
    Overflow {
        /// Number discarded since the previous poll.
        dropped: usize,
    },
}

/// A configuration, lifecycle, or outbound queue error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(&'static str);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

#[derive(Default)]
struct Events {
    queue: VecDeque<Event>,
    dropped: usize,
}

impl Events {
    fn push(&mut self, event: Event) {
        if self.queue.len() == EVENT_CAPACITY {
            self.queue.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.queue.push_back(event);
    }

    fn pop(&mut self) -> Option<Event> {
        if self.dropped != 0 {
            return Some(Event::Overflow {
                dropped: std::mem::take(&mut self.dropped),
            });
        }
        self.queue.pop_front()
    }
}

/// A nonblocking handle to one IRC connection.
///
/// Owns a dedicated background runtime. All queues are bounded; polling never
/// blocks the network loop. Dropping the handle cancels connection/registration
/// or I/O without joining a thread on the UI thread. There is no implicit retry.
pub struct Client {
    commands: mpsc::Sender<Outbound>,
    cancel: watch::Sender<bool>,
    events: Arc<Mutex<Events>>,
    state: Arc<Mutex<Arc<State>>>,
    closed: Arc<Mutex<bool>>,
    registered: Arc<AtomicBool>,
    tls: bool,
    username_len: usize,
    next_send_id: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn publish_terminal(events: &Mutex<Events>, closed: &Mutex<bool>, reason: &str) {
    let mut closed = lock(closed);
    lock(events).push(Event::Disconnected {
        reason: reason.into(),
    });
    *closed = true;
}

fn allocate_send_id(counter: &AtomicU64) -> Result<SendId, Error> {
    // Use the long-stable CAS primitive: fetch_update is deprecated on newer
    // toolchains, while its replacement is newer than our supported Rust 1.88.
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .ok_or(Error("send identifiers exhausted"))?;
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(SendId(current)),
            Err(observed) => current = observed,
        }
    }
}

impl Client {
    /// Validate settings and start connecting in the background.
    ///
    /// Success means the worker started, not that the server accepted the
    /// connection; poll for [`Event::Connected`] or [`Event::Disconnected`].
    pub fn connect(config: Config) -> Result<Self, Error> {
        validate(&config)?;
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (cancel, mut cancellation) = watch::channel(false);
        let events = Arc::new(Mutex::new(Events::default()));
        let state = Arc::new(Mutex::new(Arc::new(State::new(config.nick.clone()))));
        let closed = Arc::new(Mutex::new(false));
        let registered = Arc::new(AtomicBool::new(false));
        let client = Self {
            commands,
            cancel,
            events: events.clone(),
            state: state.clone(),
            closed: closed.clone(),
            registered: registered.clone(),
            tls: config.tls,
            username_len: config.username.len(),
            next_send_id: AtomicU64::new(1),
        };
        std::thread::Builder::new()
            .name("ruckus-network".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        publish_terminal(&events, &closed, "could not start network runtime");
                        return;
                    }
                };
                runtime.block_on(async {
                    let result = tokio::select! {
                        biased;
                        _ = cancellation.changed() => Ok(()),
                        result = async {
                            let transport = Transport::connect(&config.host, config.port, config.tls)
                                .await.map_err(|_| "connection or TLS handshake failed")?;
                            session(transport, &config, receiver, &events, &state, &registered).await
                        } => result,
                    };
                    registered.store(false, Ordering::Release);
                    publish_terminal(
                        &events, &closed, result.err().unwrap_or("connection closed"),
                    );
                });
            })
            .map_err(|_| Error("could not start network worker"))?;
        Ok(client)
    }

    /// Queue a validated IRC command without blocking.
    ///
    /// The connection must have completed registration before the worker sends
    /// user commands. A full queue is an error, never a silent dropped send.
    pub fn send(&self, command: &str, params: Vec<String>) -> Result<(), Error> {
        if command.is_empty() || !command.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return Err(Error("invalid client command"));
        }
        if !self.tls
            && matches!(
                command.to_ascii_uppercase().as_str(),
                "PASS" | "AUTHENTICATE" | "OPER"
            )
        {
            return Err(Error("credentials require TLS"));
        }
        let message = Message::new(command, params);
        let wire = message
            .serialize()
            .map_err(|_| Error("invalid outbound message"))?;
        crate::irc::validate_wire_budgets(&wire)
            .map_err(|_| Error("outbound message exceeds IRC limits"))?;
        let closed = lock(&self.closed);
        if *closed {
            return Err(Error("connection is closed"));
        }
        self.commands
            .try_send(message.into())
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => Error("outbound queue is full"),
                mpsc::error::TrySendError::Closed(_) => Error("connection is closed"),
            })
    }

    /// Join a channel using the current server-advertised CHANTYPES.
    pub fn join(&self, channel: &str) -> Result<(), Error> {
        validate_channel_with_types(channel, lock(&self.state).features.chan_types())?;
        self.send("JOIN", vec![channel.into()])
    }

    /// Leave one channel with an optional reason, using current CHANTYPES.
    pub fn part(&self, channel: &str, reason: Option<&str>) -> Result<(), Error> {
        validate_channel_with_types(channel, lock(&self.state).features.chan_types())?;
        let message =
            crate::commands::part(&[channel], reason).map_err(|_| Error("invalid PART command"))?;
        self.send(&message.command, message.params)
    }

    /// Request a nickname change using validated nickname framing.
    pub fn nick(&self, nickname: &str) -> Result<(), Error> {
        let message = crate::commands::nick(nickname).map_err(|_| Error("invalid nickname"))?;
        self.send(&message.command, message.params)
    }

    /// Send text, splitting it losslessly on UTF-8 boundaries when necessary.
    ///
    /// The relay budget reserves the server-added sender prefix. All parts are
    /// reserved in the outbound queue together or none are queued.
    pub fn privmsg(&self, target: &str, text: &str) -> Result<(), Error> {
        self.privmsg_tracked(target, text).map(|_| ())
    }

    /// Queue a logical message and obtain its later write-confirmation ID.
    ///
    /// [`Event::Sent`] is emitted only after every split fragment was flushed.
    /// Disconnection or event overflow can make the final status unknown.
    pub fn privmsg_tracked(&self, target: &str, text: &str) -> Result<SendId, Error> {
        let state = lock(&self.state);
        let user_len = if state.user.is_empty() {
            10.max(self.username_len + 1)
        } else {
            state.user.len()
        };
        let host_len = if state.host.is_empty() {
            253
        } else {
            state.host.len()
        };
        // Includes ':', '!', '@', and the trailing space.
        let prefix_len = state.nick.len() + user_len + host_len + 4;
        drop(state);
        let messages = crate::commands::privmsg(target, text, prefix_len)
            .map_err(|_| Error("invalid message or relay budget"))?;
        let closed = lock(&self.closed);
        if *closed {
            return Err(Error("connection is closed"));
        }
        let permits =
            self.commands
                .try_reserve_many(messages.len())
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => Error("outbound queue is full"),
                    mpsc::error::TrySendError::Closed(_) => Error("connection is closed"),
                })?;
        let id = allocate_send_id(&self.next_send_id)?;
        let count = messages.len();
        for (index, (permit, message)) in permits.zip(messages).enumerate() {
            permit.send(Outbound {
                message,
                completed_send: (index + 1 == count).then_some(id),
            });
        }
        Ok(id)
    }

    /// Cancel the connection, including any pending connection attempt.
    pub fn disconnect(&self) {
        *lock(&self.closed) = true;
        self.registered.store(false, Ordering::Release);
        let _ = self.cancel.send(true);
    }

    /// Whether registration completed and the connection is still open.
    ///
    /// Unlike events, this lifecycle state cannot be lost to queue overflow.
    pub fn is_registered(&self) -> bool {
        !*lock(&self.closed) && self.registered.load(Ordering::Acquire)
    }

    /// Whether the connection ended or was cancelled locally.
    pub fn is_closed(&self) -> bool {
        *lock(&self.closed)
    }

    /// Poll one event; overflow is reported before retained events.
    pub fn try_recv(&self) -> Option<Event> {
        lock(&self.events).pop()
    }

    /// Share an immutable snapshot without deep-cloning under the network lock.
    ///
    /// Later state changes use copy-on-write. Holding a snapshot cannot prevent
    /// the worker from progressing, but increases the cost of its next update.
    pub fn snapshot(&self) -> Arc<State> {
        Arc::clone(&lock(&self.state))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn validate_channel(channel: &str) -> Result<(), Error> {
    // Initial configuration is validated before ISUPPORT is available.
    validate_channel_with_types(channel, "#&+!")
}

fn validate_channel_with_types(channel: &str, channel_types: &str) -> Result<(), Error> {
    crate::commands::validate_channel(channel).map_err(|_| Error("invalid channel"))?;
    if !channel
        .chars()
        .next()
        .is_some_and(|c| channel_types.contains(c))
    {
        return Err(Error("invalid channel"));
    }
    Ok(())
}

fn validate(config: &Config) -> Result<(), Error> {
    if config.host.is_empty()
        || config.host.len() > 253
        || config.host.chars().any(char::is_whitespace)
        || config.host.chars().any(char::is_control)
        || config.port == 0
    {
        return Err(Error("invalid server address"));
    }
    if !config.tls && (config.server_password.is_some() || config.sasl.is_some()) {
        return Err(Error("credentials require TLS"));
    }
    for atom in [&config.nick, &config.username] {
        if atom.is_empty()
            || atom.starts_with(':')
            || atom.chars().any(|c| c.is_whitespace() || c.is_control())
            || atom.contains([',', '*', '?', '@', '!'])
        {
            return Err(Error("invalid identity"));
        }
    }
    if config.channels.len() > 64 {
        return Err(Error("too many initial channels"));
    }
    for channel in &config.channels {
        validate_channel(channel)?;
    }
    let mut registration = vec![
        Message::new("NICK", vec![config.nick.clone()]),
        Message::new(
            "USER",
            vec![
                config.username.clone(),
                "0".into(),
                "*".into(),
                config.realname.clone(),
            ],
        ),
    ];
    if let Some(password) = &config.server_password {
        registration.push(Message::new("PASS", vec![password.clone()]));
    }
    if let Some(credentials) = &config.sasl
        && (credentials.username.is_empty()
            || credentials.password.is_empty()
            || credentials.username.contains('\0')
            || credentials.password.contains('\0')
            || credentials.username.len() + credentials.password.len() > 4096)
    {
        return Err(Error("invalid SASL credentials"));
    }
    for message in registration {
        let wire = message
            .serialize()
            .map_err(|_| Error("invalid registration settings"))?;
        crate::irc::validate_wire_budgets(&wire)
            .map_err(|_| Error("registration settings exceed IRC limits"))?;
    }
    Ok(())
}

async fn write(transport: &mut Transport, message: Message) -> Result<(), &'static str> {
    let line = message.serialize().map_err(|_| "invalid IRC command")?;
    transport
        .write_line(&line)
        .await
        .map_err(|_| "could not send IRC command")
}

async fn actions(transport: &mut Transport, actions: Vec<cap::Action>) -> Result<(), &'static str> {
    for action in actions {
        transport
            .write_line(&format!("{}\r\n", action.to_wire()))
            .await
            .map_err(|_| "capability negotiation write failed")?;
    }
    Ok(())
}

async fn session(
    mut transport: Transport,
    config: &Config,
    mut commands: mpsc::Receiver<Outbound>,
    events: &Mutex<Events>,
    state: &Mutex<Arc<State>>,
    registration: &AtomicBool,
) -> Result<(), &'static str> {
    let mut requested = vec![
        "server-time",
        "message-tags",
        "multi-prefix",
        "batch",
        "away-notify",
        "account-notify",
        "extended-join",
        "chghost",
    ];
    if config.sasl.is_some() {
        requested.push("sasl");
    }
    let mut caps = cap::Negotiator::new(&requested);
    if let Some(password) = &config.server_password {
        write(&mut transport, Message::new("PASS", vec![password.clone()])).await?;
    }
    actions(&mut transport, caps.start()).await?;
    write(
        &mut transport,
        Message::new("NICK", vec![config.nick.clone()]),
    )
    .await?;
    write(
        &mut transport,
        Message::new(
            "USER",
            vec![
                config.username.clone(),
                "0".into(),
                "*".into(),
                config.realname.clone(),
            ],
        ),
    )
    .await?;

    let deadline = tokio::time::sleep(REGISTRATION_TIMEOUT);
    tokio::pin!(deadline);
    let mut registered = false;
    let mut sasl: Option<crate::sasl::Conversation> = None;
    let mut authenticated = false;
    loop {
        let line = tokio::select! {
            _ = &mut deadline, if !registered => return Err("registration timed out"),
            command = commands.recv(), if registered => {
                match command {
                    Some(outbound) => {
                        write(&mut transport, outbound.message).await?;
                        if let Some(id) = outbound.completed_send {
                            lock(events).push(Event::Sent { id });
                        }
                    }
                    None => return Ok(()),
                }
                continue;
            }
            line = transport.read_line() => line.map_err(|_| "IRC transport failed")?,
        };
        let Some(line) = line else {
            return Ok(());
        };
        if line.is_empty() {
            continue;
        }
        let mut message = Message::parse(&line).map_err(|_| "invalid server message")?;
        // IRC command names are ASCII case-insensitive. Canonicalize once at
        // the client boundary so every downstream frontend sees the same rule.
        message.command.make_ascii_uppercase();
        let self_nick = lock(state).nick.clone();
        match message.command.as_str() {
            "PING" => {
                write(&mut transport, Message::new("PONG", message.params.clone())).await?;
            }
            "CAP" => {
                let params: Vec<_> = message.params.iter().map(String::as_str).collect();
                let pending = caps.receive("CAP", &params);
                // Required authentication must never silently fall back.
                if config.sasl.is_some()
                    && pending
                        .iter()
                        .any(|action| matches!(action, cap::Action::End))
                    && !authenticated
                {
                    return Err("server did not accept required SASL authentication");
                }
                actions(&mut transport, pending).await?;
                if let Some(credentials) = &config.sasl
                    && caps.need_sasl()
                    && sasl.is_none()
                    && !authenticated
                {
                    let mechanism = crate::sasl::Mechanism::plain(
                        "",
                        &credentials.username,
                        &credentials.password,
                    )
                    .map_err(|_| "invalid SASL credentials")?;
                    mechanism
                        .check_offered(&caps.sasl_mechanisms())
                        .map_err(|_| "required SASL mechanism is not offered")?;
                    let mut conversation = crate::sasl::Conversation::new(mechanism);
                    let line = conversation.begin().map_err(|_| "could not start SASL")?;
                    transport
                        .write_line(&format!("{line}\r\n"))
                        .await
                        .map_err(|_| "SASL write failed")?;
                    sasl = Some(conversation);
                }
            }
            "AUTHENTICATE" | "900" | "901" | "902" | "903" | "904" | "905" | "906" | "907"
            | "908"
                if sasl.is_some() =>
            {
                let params: Vec<_> = message.params.iter().map(String::as_str).collect();
                let step = sasl
                    .as_mut()
                    .ok_or("unexpected SASL response")?
                    .receive(&message.command, &params);
                for line in step.lines {
                    transport
                        .write_line(&format!("{line}\r\n"))
                        .await
                        .map_err(|_| "SASL write failed")?;
                }
                if step.error.is_some() {
                    return Err("SASL authentication failed");
                }
                if step.success {
                    authenticated = true;
                    sasl = None;
                    actions(&mut transport, caps.sasl_complete()).await?;
                }
            }
            "AUTHENTICATE" | "901" | "902" | "903" | "904" | "905" | "906" | "907"
                if config.sasl.is_some() && !authenticated =>
            {
                return Err("SASL authentication failed");
            }
            "001" if !registered => {
                if config.sasl.is_some() && !authenticated {
                    return Err("registration completed without required SASL authentication");
                }
                registered = true;
                let nick = message
                    .params
                    .first()
                    .cloned()
                    .unwrap_or_else(|| config.nick.clone());
                // Publish the confirmed state before notifying registration.
                let affected_channels = State::apply_shared(&mut lock(state), &message);
                registration.store(true, Ordering::Release);
                {
                    let mut events = lock(events);
                    events.push(Event::Connected { nick });
                    events.push(Event::Message {
                        message,
                        affected_channels,
                        self_nick,
                    });
                }
                for channel in &config.channels {
                    write(&mut transport, Message::new("JOIN", vec![channel.clone()])).await?;
                }
                continue;
            }
            "ERROR" | "465" => return Err("server closed or refused the connection"),
            "464" if !registered => return Err("server closed or refused the connection"),
            "432" | "433" | "436" if !registered => {
                return Err("server rejected the requested nickname");
            }
            _ => {}
        }
        let affected_channels = State::apply_shared(&mut lock(state), &message);
        lock(events).push(Event::Message {
            message,
            affected_channels,
            self_nick,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_ids_are_unique_under_contention_and_exhaustion_does_not_wrap() {
        let counter = Arc::new(AtomicU64::new(1));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let counter = counter.clone();
                std::thread::spawn(move || {
                    (0..256)
                        .map(|_| allocate_send_id(&counter).unwrap().0)
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut ids: Vec<_> = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, (1..=1024).collect::<Vec<_>>());
        counter.store(u64::MAX, Ordering::Relaxed);
        assert_eq!(
            allocate_send_id(&counter),
            Err(Error("send identifiers exhausted"))
        );
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn insecure_credentials_and_injection_are_rejected_before_networking() {
        let mut config = Config {
            host: "localhost".into(),
            tls: false,
            server_password: Some("secret".into()),
            ..Config::default()
        };
        assert_eq!(validate(&config), Err(Error("credentials require TLS")));
        config.server_password = None;
        config.realname = "Ruckus\r\nOPER bad".into();
        assert!(validate(&config).is_err());
        config.realname = "Ruckus".into();
        config.channels = vec!["#ok,#injected".into()];
        assert!(validate(&config).is_err());
        let debug = format!("{config:?}");
        assert!(!debug.contains("OPER"));
    }

    #[test]
    fn bounded_events_report_loss_before_retained_messages() {
        let mut events = Events::default();
        for index in 0..EVENT_CAPACITY + 7 {
            events.push(Event::Connected {
                nick: index.to_string(),
            });
        }
        assert!(matches!(events.pop(), Some(Event::Overflow { dropped: 7 })));
        assert!(matches!(events.pop(), Some(Event::Connected { nick }) if nick == "7"));
        assert_eq!(events.queue.len(), EVENT_CAPACITY - 1);
    }

    #[test]
    fn terminal_diagnostic_is_published_before_closed_becomes_observable() {
        let events = Arc::new(Mutex::new(Events::default()));
        let closed = Arc::new(Mutex::new(false));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let guard = lock(&events);
        let worker = {
            let events = events.clone();
            let closed = closed.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                publish_terminal(&events, &closed, "registration failed");
            })
        };
        barrier.wait();
        // Publication is blocked by our events guard. Closed must either still
        // be false or be locked by the publisher, never observably true.
        if let Ok(closed) = closed.try_lock() {
            assert!(!*closed);
        }
        drop(guard);
        worker.join().unwrap();
        assert!(*lock(&closed));
        assert!(matches!(
            lock(&events).pop(),
            Some(Event::Disconnected { reason }) if reason == "registration failed"
        ));
    }

    #[test]
    fn debug_redacts_credentials() {
        let config = Config {
            server_password: Some("server-secret".into()),
            sasl: Some(SaslCredentials {
                username: "account-secret".into(),
                password: "password-secret".into(),
            }),
            ..Config::default()
        };
        let debug = format!("{config:?}");
        for secret in ["server-secret", "account-secret", "password-secret"] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn outbound_queue_is_bounded_and_failures_are_visible() {
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (cancel, _cancellation) = watch::channel(false);
        let client = Client {
            commands,
            cancel,
            events: Arc::new(Mutex::new(Events::default())),
            state: Arc::new(Mutex::new(Arc::new(State::new("ruckus")))),
            closed: Arc::new(Mutex::new(false)),
            registered: Arc::new(AtomicBool::new(false)),
            tls: false,
            username_len: 6,
            next_send_id: AtomicU64::new(1),
        };
        assert_eq!(
            client.send("pass", vec!["secret".into()]),
            Err(Error("credentials require TLS"))
        );
        assert_eq!(
            client.send("oPeR", vec!["operator".into(), "secret".into()]),
            Err(Error("credentials require TLS"))
        );
        assert!(client.send("P!NG", vec![]).is_err());
        assert!(client.send("1abc", vec![]).is_err());
        assert!(client.privmsg("#test", "injected\r\nQUIT").is_err());
        for _ in 0..COMMAND_CAPACITY - 1 {
            client.privmsg("#test", "hello").unwrap();
        }
        assert_eq!(
            client.privmsg("#test", &"é".repeat(200)),
            Err(Error("outbound queue is full"))
        );
        assert_eq!(receiver.len(), COMMAND_CAPACITY - 1);
        client.privmsg("#test", "last single line").unwrap();
        assert_eq!(
            client.privmsg("#test", "retry me"),
            Err(Error("outbound queue is full"))
        );
        client.disconnect();
        assert!(client.is_closed());
        assert!(!client.is_registered());
        assert_eq!(
            client.send("PING", vec![]),
            Err(Error("connection is closed"))
        );
        assert_eq!(
            client.privmsg("#test", "after disconnect"),
            Err(Error("connection is closed"))
        );
    }

    #[test]
    fn channel_actions_use_live_chantypes_without_relaxing_framing() {
        let (commands, mut receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (cancel, _cancellation) = watch::channel(false);
        let client = Client {
            commands,
            cancel,
            events: Arc::new(Mutex::new(Events::default())),
            state: Arc::new(Mutex::new(Arc::new(State::new("ruckus")))),
            closed: Arc::new(Mutex::new(false)),
            registered: Arc::new(AtomicBool::new(true)),
            tls: false,
            username_len: 6,
            next_send_id: AtomicU64::new(1),
        };
        assert_eq!(client.join("~custom"), Err(Error("invalid channel")));
        State::apply_shared(
            &mut lock(&client.state),
            &Message::new(
                "005",
                vec!["ruckus".into(), "CHANTYPES=#~".into(), "supported".into()],
            ),
        );
        client.join("~custom").unwrap();
        client.part("~custom", Some("leaving")).unwrap();
        assert_eq!(
            receiver.try_recv().unwrap().message,
            Message::new("JOIN", vec!["~custom".into()])
        );
        assert_eq!(
            receiver.try_recv().unwrap().message,
            Message::new("PART", vec!["~custom".into(), "leaving".into()])
        );
        for channel in [
            "#bad:colon",
            "~bad:colon",
            "#bad,comma",
            "#bad space",
            "#bad\t",
            "#bad\r\nQUIT",
            "#bad\0",
            "#bad\u{7}",
            "&unsupported",
            "~",
            "",
        ] {
            assert_eq!(
                client.join(channel),
                Err(Error("invalid channel")),
                "{channel:?}"
            );
            assert_eq!(
                client.part(channel, None),
                Err(Error("invalid channel")),
                "{channel:?}"
            );
        }
        assert!(receiver.try_recv().is_err());
        // Initial settings retain the standard prefixes before negotiation.
        for channel in ["#standard", "&local", "+modeless", "!safe"] {
            validate_channel(channel).unwrap();
        }
        assert_eq!(validate_channel("~custom"), Err(Error("invalid channel")));
    }

    async fn expect(reader: &mut tokio::io::BufReader<tokio::io::DuplexStream>, expected: &str) {
        use tokio::io::AsyncBufReadExt;
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .expect("script timeout")
            .expect("read client command");
        assert_eq!(line, expected);
    }

    #[tokio::test]
    async fn tracked_send_confirms_only_after_all_fragments_and_not_on_write_failure() {
        use tokio::io::{AsyncWriteExt, BufReader};
        for complete in [true, false] {
            let (client, server) = tokio::io::duplex(64);
            let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
            let events = Mutex::new(Events::default());
            let state = Mutex::new(Arc::new(State::new("ruckus")));
            let registration = AtomicBool::new(false);
            let config = Config::default();
            let fragments = crate::commands::privmsg("#test", &"hello ".repeat(150), 200).unwrap();
            assert!(fragments.len() > 1);
            let script = async {
                let mut server = BufReader::new(server);
                expect(&mut server, "CAP LS 302\r\n").await;
                expect(&mut server, "NICK ruckus\r\n").await;
                expect(&mut server, "USER ruckus 0 * Ruckus\r\n").await;
                server.write_all(b":s CAP * LS :\r\n").await.unwrap();
                expect(&mut server, "CAP END\r\n").await;
                server
                    .write_all(b":s 001 ruckus :welcome\r\nPING ready\r\n")
                    .await
                    .unwrap();
                expect(&mut server, "PONG ready\r\n").await;
                for (index, message) in fragments.iter().cloned().enumerate() {
                    commands
                        .send(Outbound {
                            message,
                            completed_send: (index + 1 == fragments.len()).then_some(SendId(9)),
                        })
                        .await
                        .unwrap();
                }
                expect(&mut server, &fragments[0].serialize().unwrap()).await;
                assert!(
                    !lock(&events)
                        .queue
                        .iter()
                        .any(|event| matches!(event, Event::Sent { .. }))
                );
                if !complete {
                    return;
                }
                for message in &fragments[1..] {
                    expect(&mut server, &message.serialize().unwrap()).await;
                }
                server.write_all(b"PING after\r\n").await.unwrap();
                expect(&mut server, "PONG after\r\n").await;
                assert_eq!(
                    lock(&events)
                        .queue
                        .iter()
                        .filter(|event| matches!(event, Event::Sent { id: SendId(9) }))
                        .count(),
                    1
                );
            };
            let (result, ()) = tokio::join!(
                session(
                    Transport::from_stream(client),
                    &config,
                    receiver,
                    &events,
                    &state,
                    &registration
                ),
                script,
            );
            assert_eq!(result.is_ok(), complete);
            assert_eq!(
                lock(&events)
                    .queue
                    .iter()
                    .filter(|event| matches!(event, Event::Sent { .. }))
                    .count(),
                usize::from(complete)
            );
        }
    }

    #[test]
    fn tracked_message_ids_complete_only_the_last_queued_fragment() {
        let (commands, mut receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (cancel, _cancellation) = watch::channel(false);
        let client = Client {
            commands,
            cancel,
            events: Arc::new(Mutex::new(Events::default())),
            state: Arc::new(Mutex::new(Arc::new(State::new("exec")))),
            closed: Arc::new(Mutex::new(false)),
            registered: Arc::new(AtomicBool::new(true)),
            tls: false,
            username_len: 4,
            next_send_id: AtomicU64::new(1),
        };
        let id = client
            .privmsg_tracked("#ruckus", &"long text ".repeat(80))
            .unwrap();
        let mut fragments = Vec::new();
        while let Ok(message) = receiver.try_recv() {
            fragments.push(message);
        }
        assert!(fragments.len() > 1);
        assert_eq!(fragments.last().unwrap().completed_send, Some(id));
        assert!(
            fragments[..fragments.len() - 1]
                .iter()
                .all(|fragment| fragment.completed_send.is_none())
        );
        let next = client.privmsg_tracked("#ruckus", "next").unwrap();
        assert_ne!(next, id);
        assert_eq!(receiver.try_recv().unwrap().completed_send, Some(next));
        client.next_send_id.store(u64::MAX, Ordering::Relaxed);
        assert_eq!(
            client.privmsg_tracked("#ruckus", "not queued"),
            Err(Error("send identifiers exhausted"))
        );
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn registration_has_a_deadline_even_when_server_stays_silent() {
        let (client, server) = tokio::io::duplex(4096);
        let (_commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let events = Mutex::new(Events::default());
        let state = Mutex::new(Arc::new(State::new("ruckus")));
        let registration = AtomicBool::new(false);
        let config = Config::default();
        let script = async {
            let mut server = tokio::io::BufReader::new(server);
            expect(&mut server, "CAP LS 302\r\n").await;
            expect(&mut server, "NICK ruckus\r\n").await;
            expect(&mut server, "USER ruckus 0 * Ruckus\r\n").await;
            tokio::time::advance(REGISTRATION_TIMEOUT + Duration::from_secs(1)).await;
            // Retain the peer until the session times out; EOF is not the cause.
            std::future::pending::<()>().await;
        };
        tokio::pin!(script);
        let result = tokio::select! {
            result = session(
                Transport::from_stream(client), &config, receiver,
                &events, &state, &registration,
            ) => result,
            _ = &mut script => unreachable!("script deliberately stays open"),
        };
        assert_eq!(result, Err("registration timed out"));
        assert!(!registration.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn registration_status_survives_event_overflow() {
        use tokio::io::AsyncWriteExt;
        let (client, server) = tokio::io::duplex(4096);
        let (_commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let events = Mutex::new(Events::default());
        let state = Mutex::new(Arc::new(State::new("requested")));
        let registration = AtomicBool::new(false);
        let config = Config::default();
        let script = async {
            let mut server = tokio::io::BufReader::new(server);
            expect(&mut server, "CAP LS 302\r\n").await;
            expect(&mut server, "NICK ruckus\r\n").await;
            expect(&mut server, "USER ruckus 0 * Ruckus\r\n").await;
            server.write_all(b":s CAP * LS :\r\n").await.unwrap();
            expect(&mut server, "CAP END\r\n").await;
            server
                .write_all(b":s 001 confirmed :welcome\r\n")
                .await
                .unwrap();
            server.write_all(b"PING :before\r\n").await.unwrap();
            expect(&mut server, "PONG before\r\n").await;
            let published = Arc::clone(&lock(&state));
            for _ in 0..EVENT_CAPACITY + 5 {
                server
                    .write_all(b":s NOTICE confirmed :burst\r\n")
                    .await
                    .unwrap();
            }
            server.write_all(b"PING :after\r\n").await.unwrap();
            expect(&mut server, "PONG after\r\n").await;
            assert!(
                Arc::ptr_eq(&published, &lock(&state)),
                "chat and liveness traffic must not deep-copy published state"
            );
        };
        let (result, ()) = tokio::join!(
            session(
                Transport::from_stream(client),
                &config,
                receiver,
                &events,
                &state,
                &registration,
            ),
            script
        );
        assert!(result.is_ok());
        assert!(registration.load(Ordering::Acquire));
        assert_eq!(lock(&state).nick, "confirmed");
        assert!(
            !lock(&events)
                .queue
                .iter()
                .any(|event| matches!(event, Event::Connected { .. }))
        );
        assert!(matches!(lock(&events).pop(), Some(Event::Overflow { .. })));
    }

    #[tokio::test]
    async fn scripted_registration_ping_and_join() {
        use tokio::io::{AsyncWriteExt, BufReader};
        let (client, server) = tokio::io::duplex(16 * 1024);
        let config = Config {
            host: "localhost".into(),
            tls: false,
            channels: vec!["#rust".into()],
            ..Config::default()
        };
        let (_commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let events = Mutex::new(Events::default());
        let state = Mutex::new(Arc::new(State::new("ruckus")));
        let initial_snapshot = Arc::clone(&lock(&state));
        let registration = AtomicBool::new(false);
        let script = async {
            let mut server = BufReader::new(server);
            expect(&mut server, "CAP LS 302\r\n").await;
            expect(&mut server, "NICK ruckus\r\n").await;
            expect(&mut server, "USER ruckus 0 * Ruckus\r\n").await;
            server
                .write_all(b":server CAP * LS :server-time multi-prefix\r\n")
                .await
                .unwrap();
            // Capability order follows the configured request order.
            expect(&mut server, "CAP REQ :server-time multi-prefix\r\n").await;
            server
                .write_all(b":server CAP * ACK :server-time multi-prefix\r\n")
                .await
                .unwrap();
            expect(&mut server, "CAP END\r\n").await;
            server
                .write_all(b"\r\nping :token\r\n:server 001 ruckus_ :Welcome\r\n")
                .await
                .unwrap();
            expect(&mut server, "PONG token\r\n").await;
            expect(&mut server, "JOIN #rust\r\n").await;
            assert!(registration.load(Ordering::Acquire));
            assert_eq!(lock(&state).nick, "ruckus_");
            server
                .write_all(
                    b":ruckus_!u@h join #rust\r\n:other!u@h JOIN #rust\r\n:other!u@h NICK renamed\r\n:renamed!u@h QUIT :bye\r\n",
                )
                .await
                .unwrap();
        };
        let (result, ()) = tokio::join!(
            session(
                Transport::from_stream(client),
                &config,
                receiver,
                &events,
                &state,
                &registration
            ),
            script
        );
        assert!(result.is_ok());
        assert!(
            lock(&events)
                .queue
                .iter()
                .any(|event| matches!(event, Event::Connected { nick } if nick == "ruckus_"))
        );
        assert_eq!(initial_snapshot.nick, "ruckus");
        assert_eq!(lock(&state).nick, "ruckus_");
        assert!(lock(&state).channel("#rust").is_some());
        for command in ["NICK", "QUIT"] {
            assert!(lock(&events).queue.iter().any(|event| matches!(
                event,
                Event::Message { message, affected_channels, .. }
                    if message.command == command && affected_channels == &["#rust"]
            )));
        }
    }

    #[tokio::test]
    async fn password_mismatch_is_terminal_only_during_registration() {
        use tokio::io::{AsyncWriteExt, BufReader};
        for registered in [false, true] {
            let (client, server) = tokio::io::duplex(16 * 1024);
            let config = Config::default();
            let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
            let events = Mutex::new(Events::default());
            let state = Mutex::new(Arc::new(State::new("ruckus")));
            let registration = AtomicBool::new(false);
            let script = async {
                let mut server = BufReader::new(server);
                expect(&mut server, "CAP LS 302\r\n").await;
                expect(&mut server, "NICK ruckus\r\n").await;
                expect(&mut server, "USER ruckus 0 * Ruckus\r\n").await;
                server.write_all(b":server CAP * LS :\r\n").await.unwrap();
                expect(&mut server, "CAP END\r\n").await;
                if registered {
                    server
                        .write_all(b":server 001 ruckus :Welcome\r\nPING ready\r\n")
                        .await
                        .unwrap();
                    expect(&mut server, "PONG ready\r\n").await;
                    // Exercise the worker queue directly over a hermetic stream;
                    // the public API's plaintext OPER guard remains unchanged.
                    commands
                        .send(Message::new("OPER", vec!["operator".into(), "secret".into()]).into())
                        .await
                        .unwrap();
                    expect(&mut server, "OPER operator secret\r\n").await;
                }
                server
                    .write_all(b":server 464 ruckus :Password incorrect\r\nPING alive\r\n")
                    .await
                    .unwrap();
                if registered {
                    expect(&mut server, "PONG alive\r\n").await;
                    assert!(registration.load(Ordering::Acquire));
                }
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(
                    session(
                        Transport::from_stream(client),
                        &config,
                        receiver,
                        &events,
                        &state,
                        &registration
                    ),
                    script
                )
            })
            .await
            .unwrap();
            if registered {
                assert!(result.is_ok());
                assert!(lock(&events).queue.iter().any(|event| matches!(
                    event, Event::Message { message, .. } if message.command == "464"
                )));
            } else {
                assert_eq!(result, Err("server closed or refused the connection"));
                assert!(!registration.load(Ordering::Acquire));
            }
        }
    }

    async fn sasl_script(advertisement: &str, verdict: &str) -> Result<(), &'static str> {
        use tokio::io::{AsyncWriteExt, BufReader};
        let (client, server) = tokio::io::duplex(16 * 1024);
        let config = Config {
            host: "in-process.test".into(),
            sasl: Some(SaslCredentials {
                username: "user".into(),
                password: "pass".into(),
            }),
            ..Config::default()
        };
        let (_commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let events = Mutex::new(Events::default());
        let state = Mutex::new(Arc::new(State::new("ruckus")));
        let registration = AtomicBool::new(false);
        let script = async {
            let mut server = BufReader::new(server);
            expect(&mut server, "CAP LS 302\r\n").await;
            expect(&mut server, "NICK ruckus\r\n").await;
            expect(&mut server, "USER ruckus 0 * Ruckus\r\n").await;
            server
                .write_all(format!(":s CAP * LS :{advertisement}\r\n").as_bytes())
                .await
                .unwrap();
            if !advertisement.contains("sasl") {
                return;
            }
            expect(&mut server, "CAP REQ :sasl\r\n").await;
            server.write_all(b":s CAP * ACK :sasl\r\n").await.unwrap();
            if advertisement == "sasl=EXTERNAL" {
                return;
            }
            expect(&mut server, "AUTHENTICATE PLAIN\r\n").await;
            server.write_all(b"AUTHENTICATE +\r\n").await.unwrap();
            expect(&mut server, "AUTHENTICATE AHVzZXIAcGFzcw==\r\n").await;
            server
                .write_all(format!(":s {verdict} ruckus :result\r\n").as_bytes())
                .await
                .unwrap();
            if verdict == "903" {
                expect(&mut server, "CAP END\r\n").await;
                server
                    .write_all(b":s 001 ruckus :welcome\r\n")
                    .await
                    .unwrap();
            }
        };
        // The injected transport is trusted test infrastructure; no TLS is
        // bypassed by the production Client::connect API.
        let (result, ()) = tokio::join!(
            session(
                Transport::from_stream(client),
                &config,
                receiver,
                &events,
                &state,
                &registration
            ),
            script
        );
        let connected = lock(&events)
            .queue
            .iter()
            .any(|event| matches!(event, Event::Connected { .. }));
        assert_eq!(connected, verdict == "903" && advertisement == "sasl=PLAIN");
        result
    }

    #[tokio::test]
    async fn required_sasl_success_failure_and_no_downgrade() {
        assert!(sasl_script("sasl=PLAIN", "903").await.is_ok());
        assert_eq!(
            sasl_script("sasl=PLAIN", "904").await,
            Err("SASL authentication failed")
        );
        for numeric in ["901", "902", "905", "906", "907"] {
            assert_eq!(
                sasl_script("sasl=PLAIN", numeric).await,
                Err("SASL authentication failed")
            );
        }
        assert_eq!(
            sasl_script("sasl=EXTERNAL", "903").await,
            Err("required SASL mechanism is not offered")
        );
        assert_eq!(
            sasl_script("", "903").await,
            Err("server did not accept required SASL authentication")
        );
    }
}
