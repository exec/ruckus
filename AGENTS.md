# Working agreement for Ruckus

Ruckus is a Rust IRCv3 client based on the Go client Lurk. Read
`docs/ARCHITECTURE.md` before changing the architecture. The project is currently
an early port; consult `docs/PORTING.md` and do not describe planned behavior as
implemented.

## Boundaries

- `crates/ruckus-core` is the reusable, UI-independent client library.
- `crates/ruckus` is the GPUI desktop executable and frontend wiring.
- Keep GPUI and any future terminal UI dependencies out of the core.
- Start with modules; add crates only when a concrete boundary needs one.
- The core uses Tokio and rustls; the desktop uses GPUI and gpui-component.
  Add dependencies deliberately, not speculatively.
- Never send credentials over plaintext by default. Never log secrets.
- Treat IRC input as hostile: bound framing and queues, reject wire injection,
  and sanitize server-controlled text before terminal rendering.
- Avoid panics on library error paths; document public APIs and preserve
  upstream license notices in translated code.

## Verification

Every new behavior needs a hermetic test. Use scripted in-process transports
for networking and synthetic events for UI tests. Live IRC servers must never
be required for the default suite. Do not connect to public IRC networks as
part of automated validation.

Run before handing off:

```sh
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo doc --workspace --no-deps --locked
```

Keep `Cargo.lock` committed for the application. Keep the declared minimum Rust
version and CI in sync; do not raise the minimum incidentally.
