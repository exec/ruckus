# Contributing

Ruckus is an early client port. Keep changes small, tested, and explicit about
which Lurk behavior they preserve or intentionally change.

1. Read [the architecture](docs/ARCHITECTURE.md) and [AGENTS.md](AGENTS.md).
2. Add hermetic tests with new behavior; no public IRC server is needed.
3. Run the checks listed in the README.
4. Describe observable changes and any remaining limitations.

The minimum supported Rust version is 1.88, required by GPUI's let-chain syntax.
CI checks that version for the headless workspace and current stable Rust for
the desktop build. Dependency additions should explain their purpose and
effect on the core/frontend boundary. Preserve attribution and license notices
for code adapted from Lurk or elsewhere.
