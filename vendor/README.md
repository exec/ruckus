# Patched dependencies

## gpui-component 0.5.1

This directory contains the unmodified published crate archive except for the
small `InputState::render` fix described below. Its Apache-2.0 license is retained
in `gpui-component-0.5.1/LICENSE-APACHE`.

- Upstream: https://github.com/longbridge/gpui-component
- Published version: `0.5.1`, compatible with `gpui 0.2.2`
- Crate archive SHA-256:
  `d021d46b4088d3d93a57ccdf443da85695a77272108caca2f6fe5369f584966a`
- Functional patch: add `.flex()` to the `InputState::render` root in
  `src/input/state.rs`.

### Why the patch exists

The inner text element uses percentage width. In Ruckus's nested field layout,
the upstream block container is wide but its text viewport resolves to zero
width. Normal caret scrolling then shifts even short input off the left edge.
A flex formatting context makes the text viewport fill the existing control.

Native synthetic-window regressions exercise painted hit geometry while typing,
not just the stored string. They fail with the pristine dependency and pass with
this patch. No editing, selection, clipboard, IME, or scrolling implementation is
replaced.

There is no published 0.5.2; the 0.6 series switches to `gpui-pre` and is not a
drop-in fix for this project. This path override is intentionally explicit and
reproducible; do not edit the Cargo registry cache. When upgrading the UI stack,
verify the regression against upstream and remove this copy if the patch is no
longer needed. Avoid reformatting or otherwise modifying third-party source.
