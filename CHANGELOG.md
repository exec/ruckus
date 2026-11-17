# Changelog

## v0.2 — Efficiency and conversation design

This remains an early client port, not full Lurk feature parity. This is a source
release; signed application bundles and installers are not included.

### Changed

- Idle desktop polls skip reconciliation of unchanged immutable snapshots instead
  of repeatedly allocating feature/identity strings and scanning joined channels.
  New snapshots still refresh member/topic-only changes, and lifecycle changes
  remain independent of the droppable event queue.
- Clearer offline, connecting, and registered workspace states, with actionable
  connection/join controls and accurate TLS configuration wording.
- Better narrow-window containment and outgoing-message status presentation.
- An actionable composer connection rail remains available above retained history;
  reopening settings preserves the draft and selects a visible input.
- State-specific command guidance, functional conversation descriptions, and a
  shared alignment grid replace ambiguous icons and decorative filler.
- Outgoing history retains exact send identity through bounded-history eviction
  and casemapping merges; late confirmations cannot mark an identical newer
  message as sent.
- History projections invalidate after structural casemapping changes, even when
  append/edit counters happen to match their previous values.
- CI caches compiled dependencies as well as downloads, separates compiler,
  platform, feature, vendor, and macOS native-toolchain inputs, cancels superseded
  runs, and avoids repeating branch checks for release tags. Warm-cache speedups
  have not yet been measured.

### Verification and limits

Hermetic protocol/model tests and synthetic native-window tests cover the changed
behavior. The macOS desktop and headless MSRV/stable CI remain the supported
verification paths. No new Linux/Windows GUI support or full live-network parity
is claimed. Existing reconnect, history integration, multi-network, and other
porting gaps remain documented in [PORTING.md](docs/PORTING.md).

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
