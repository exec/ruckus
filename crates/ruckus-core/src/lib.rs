//! UI-independent IRCv3 client core for Ruckus, adapted from Lurk.
//!
//! Protocol and state reducers can be used without a runtime. [`client::Client`]
//! owns its background network runtime and exposes bounded nonblocking queues.
//! Raw remote text must be sanitized at the rendering boundary.

pub mod cap;
pub mod chatlog;
pub mod client;
pub mod commands;
pub mod config;
pub mod ctcp;
pub mod history;
pub mod irc;
pub mod isupport;
pub mod sanitize;
pub mod sasl;
pub mod state;
pub mod transport;
