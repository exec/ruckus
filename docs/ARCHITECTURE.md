# Architecture

Ruckus is a Rust IRCv3 desktop client based on the MIT-licensed Go client
[Lurk](https://github.com/exec/lurk). GPUI is the primary frontend; a future TUI
can reuse the core. This is an early port, not full Lurk feature parity.

## Workspace and dependency direction

```text
crates/ruckus/        GPUI desktop, pure frontend model, CLI entry point
        |
        v
crates/ruckus-core/   wire protocol, transport, registration, state, helpers
```

Both packages use edition 2024 and Rust 1.88 minimum (required by GPUI syntax).
The core has no GPUI or terminal UI dependency. Start with modules rather than
adding a crate per Go package.

Core modules:

- `irc`: owned messages, IRCv3 tag encoding, injection validation, wire budgets.
- `transport`: bounded cancellation-safe framing, TCP and verified rustls TLS.
- `cap`, `isupport`, `sasl`: protocol state machines independent of the UI.
- `state`: deterministic channel/member/topic reducer using server casemapping.
- `client`: registration and network worker, bounded commands/events, snapshots.
- `commands`: validated action builders and UTF-8-safe message splitting.
- `sanitize`, `ctcp`: display-control filtering and strict CTCP parsing.
- `config`, `chatlog`: persistence helpers, independent of the network runtime.
- `history`: bounded nested BATCH classification and CHATHISTORY builders.

The frontend's pure model owns conversation histories and slash-command parsing.
GPUI owns window/input/focus/rendering, while membership and topics come from
core snapshots—not a competing frontend IRC state machine. The default `gui`
feature selects GPUI; disabling it leaves help/version and headless model tests,
not a line-mode IRC client.

### Desktop presentation

`desktop.rs` owns connection/input events and view state; `desktop/view.rs`
composes welcome, conversation, sidebar, composer, and member views. `theme.rs`
is the shared dark palette for both shell surfaces and gpui-component controls.
The application header is also the client-side title bar: macOS keeps native
traffic lights on the left, while gpui-component supplies Windows/Linux controls
on the right and platform window dragging. `Root` supplies Linux resize borders.
Window-control SVGs are embedded by the desktop asset source rather than loaded
from the working directory. Windows/Linux native interaction remains unverified.
`presentation.rs` losslessly projects sanitized history lines into author,
outgoing-status, and system rows. Nickname initials/colors are decorative, not verified
identity or presence. No timestamps or network activity are fabricated.

Tracked sends have connection-local IDs. A logical message is marked sent only
after every split fragment is written/flushed; no delivery receipt is inferred.
The bounded frontend pending map transitions queued rows in place and marks
missing confirmations unconfirmed on overflow/disconnection. In-place edits use
a separate cache revision from appends so status changes preserve scroll anchors.

The connection card collapses after registration and can be reopened without
ending the session. Advanced credentials remain masked. Conversation history
and the draft survive view toggles. Rendering keeps the existing frame/event,
history, and member-display bounds.
The variable-height GPUI message list shapes only the visible rows plus a small
overdraw region. A revision-keyed shared history cache avoids reparsing and
cloning the whole conversation on unchanged frames; ring updates preserve the
logical scroll anchor while older retained messages remain available.

## Runtime and ownership

`Client::connect` validates settings synchronously and starts a dedicated thread
with a Tokio runtime. Success means the worker started; registration completion
arrives separately as `Event::Connected`. The UI never blocks on network I/O.

Each connection has a 64-command queue and a 512-event queue. Full outbound
queues return an error. Inbound overflow discards oldest events and reports
the loss before retained events; the authoritative core state still updates.
Registration/closed status is available separately and cannot be dropped.
The GUI polls at bounded intervals and caps work per frame. Immutable `Arc`
snapshots keep UI reads constant-time under the worker lock; state changes use
copy-on-write. Ordinary chat/liveness traffic does not copy state. Membership is
bounded to 65,536 entries across at most 256 joined channels.
The desktop reconciles snapshots only when their shared identity or lifecycle
changes, or retained FIFO events temporarily replace the authoritative identity.
An unchanged idle snapshot does not rebuild joined-channel projections. New
snapshots invalidate the view even when only topics or members changed.

Connection setup and registration have deadlines. Reads have no default idle
timeout. Framing retains partial bytes across cancellation. A cancelled or
failed write poisons the transport rather than reusing a partially written
IRC stream. Dropping a client cancels its worker without joining on the UI thread.
Automatic reconnection is not implemented yet.

## Security boundaries

- TLS uses Mozilla trust roots with certificate/hostname verification. There is
  no production certificate-verification bypass.
- Client configuration refuses credentials on plaintext connections. Configured
  SASL is required: unsupported/rejected authentication cannot silently downgrade.
- Body frames are bounded to 512 bytes including CRLF, with a separate 8191-byte
  combined server tag section. Input is strict CRLF and UTF-8 at transport.
- Outbound messages reject CR/LF/NUL injection. Raw messages and snapshots retain
  remote text; all displayed remote strings pass through sanitization.
- Credential-bearing configuration and authentication objects redact Debug.
  Generated SASL wire strings necessarily contain encoded secrets: never log them.
- Configuration/log helpers use restrictive Unix file modes and reject static
  symlinks. Filesystem parents must be trusted; portable checks do not prevent a
  hostile concurrent parent-directory replacement. Windows ACLs are not verified.

## Reference and verification

Lurk's `irc`, `conn`, `cap`, `sasl`, `isupport`, `client`, `config`, and `chatlog`
packages and hermetic tests are behavioral references. We translate contracts,
not Go concurrency primitives line for line. Preserve upstream MIT attribution.

Tests cover pure protocol/state transitions, synthetic frontend events, scripted
in-process client/server exchanges, fragmented reads, cancellation, budgets,
authentication failure, and generated TLS certificates. No default test connects
to a public IRC server or requires a graphical session.
The optional `ui-tests` feature enables GPUI's synthetic native-window support
without bringing that support into ordinary or headless builds.
Input regressions assert painted character geometry as well as stored text,
including burst typing, overflow/home, resizing, masked fields, and focus handoff.
The pinned `gpui-component` layout compatibility patch is documented in
[`vendor/README.md`](../vendor/README.md); keep its upstream source otherwise intact.

See [PORTING.md](PORTING.md) for scope and gaps. The `lurkd` daemon is not part
of this client port; connecting to a bouncer as a client is a separate concern.
