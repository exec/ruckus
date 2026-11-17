# Ruckus

A Rust IRCv3 **desktop client built with GPUI**, based on
[Lurk](https://github.com/exec/lurk).

**Status: early client port, not feature parity with Lurk.** The desktop frontend
connects to a server, opens conversation buffers, displays channel members and
topics, and sends messages. The reusable core implements wire messages/tags,
bounded TCP/TLS transport, registration/CAP negotiation, SASL PLAIN, ISUPPORT,
and channel/member state. Configuration, logging, and history helpers are
available in the library but not yet fully wired into the desktop workflow.

See the [porting status](docs/PORTING.md) for implemented pieces and remaining
gaps. Lurk is unchanged; Ruckus is not yet a drop-in replacement.

## Run

```sh
cargo run -p ruckus --locked
```

Enter a server hostname, port, nickname, and optional initial channel. TLS is
enabled by default. Optional SASL PLAIN login requires both account and password
and verified TLS. No connection is made until you click Connect.

The sidebar selects conversations; the composer supports plain messages and
`/join #channel`, `/part [#channel]`, `/msg nick text`, `/nick nick`, and `/quit`.
Connection setup lives in the welcome card; **Connection** reopens it during a
session. Authentication and the port/TLS controls are under **Authentication &
advanced**. The member-count button shows or hides the member panel.
Outgoing messages advance from **Queued** to **Sent** after all their wire
fragments are written and flushed. **Sent** is not a server acknowledgement or
recipient delivery receipt. If the connection ends or confirmation events are
lost, pending rows become **Unconfirmed**. A failed enqueue leaves the draft intact.
`/quit` and Disconnect currently cancel immediately; they do not perform a
graceful IRC QUIT exchange.

## Development

Install Rust 1.88 or newer (edition 2024), including `rustfmt` and `clippy`.
The minimum was raised deliberately because GPUI uses let-chain syntax.
The desktop uses published `gpui` 0.2.2 and matching `gpui-component` 0.5.1 for
native text editing, selection, clipboard, and IME support.
`gpui-component` has a small, documented [local layout patch](vendor/README.md)
for zero-width input text viewports; builds do not depend on registry-cache edits.

The first desktop build targets macOS. GPUI's `runtime_shaders` feature avoids
requiring the offline Apple Metal compiler at build time; a working graphical
session and Metal-capable device are still required to run. Linux needs GPUI's
native graphics/windowing libraries; Linux/Windows desktop builds are not yet
verified. Core and headless tests do not require a display.

```sh
cargo build --workspace --locked
cargo run -p ruckus -- --help
cargo run -p ruckus -- --version
# Library + headless frontend model/CLI checks without desktop dependencies:
cargo test --workspace --no-default-features --locked
# Optional synthetic native-window/input tests (no IRC server or display needed):
cargo test -p ruckus --features ui-tests --locked desktop::tests
```

Before submitting changes:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo doc --workspace --no-deps --locked
```

Network tests use scripted in-process transports, including TLS certificate
verification tests. They never require a public IRC network.

See [the architecture](docs/ARCHITECTURE.md) and
[contribution guidelines](CONTRIBUTING.md).

## License and origins

MIT; see [LICENSE](LICENSE). Ruckus takes its initial architectural direction
from Lurk, also MIT-licensed, copyright (c) 2026 exec. Preserve upstream notices
when translating or adapting code, and credit non-obvious borrowed designs.
