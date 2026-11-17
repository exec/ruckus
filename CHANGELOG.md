# Changelog

## v0.1 — Initial desktop client

Ruckus is an early Rust IRCv3 client based on Lurk, with a GPUI desktop frontend
and a reusable UI-independent core. This release is not yet a drop-in replacement
for Lurk.

### Included

- Dark, conversation-first desktop workspace with connection setup, channel/DM
  navigation, virtualized chat history, topics, member lists, and native input.
- TCP and verified TLS, CAP negotiation, required SASL PLAIN authentication,
  IRCv3 message tags, ISUPPORT/casemapping, and channel/member state tracking.
- Basic `/join`, `/part`, `/msg`, `/nick`, and `/quit` commands.
- Tracked outgoing messages with Queued, Sent, and Unconfirmed states. Sent means
  written/flushed to the connection, not a recipient delivery receipt.
- Reusable configuration/logging helpers, CTCP parsing, BATCH tracking, and
  CHATHISTORY request builders.
- Bounded input/queues/state, wire-injection validation, secret redaction, and
  display sanitization.
- Hermetic core and native synthetic-window tests; CI for headless Rust 1.88 and
  stable builds plus the macOS desktop.
- MIT project license, Lurk attribution, and a documented vendored Apache-2.0
  gpui-component patch for input text viewport layout.

### Build and run

Requires Rust 1.88 or newer. The desktop has been built and tested on macOS.
This is a source release; no signed application bundle or installer is included.

```sh
cargo run -p ruckus --locked
```

### Known limits

Automatic reconnect/rejoin, multi-network sessions, saved-network/logging UI,
history paging/recovery, SCRAM, typing indicators, and richer commands remain
future work. Linux/Windows desktop builds are unverified. `/quit` currently
cancels the transport immediately rather than guaranteeing a graceful IRC QUIT.

See [the porting matrix](docs/PORTING.md) for library-versus-desktop support.
Local test servers, runtime data, credentials, and build artifacts are not part
of this release.
