# Draftmux

Draftmux is a macOS app for drafting GitHub and Slack messages with an agent.
Hand off from a pi or Claude Code session, polish the message next to a
GitHub or Slack preview with an embedded pi agent and steer-style selection
actions, then copy it out or let the original agent read it back.

Status: early development. See [docs/design.md](docs/design.md) for the design and
[docs/prototype/](docs/prototype/) for HTML prototypes of the main window and
settings.

## Development

Requires macOS with full Xcode installed (gpui compiles its Metal shaders at
build time). The Rust toolchain is pinned in `rust-toolchain.toml`.

```sh
cargo run
```

### Layout

```text
Cargo.toml           workspace manifest; shared dependencies and the gpui pin
rust-toolchain.toml  toolchain matching Zed at the pinned gpui rev
crates/draftmux/     the app binary: startup, window and UI
crates/markup/       message block tree and its parsers (no gpui)
crates/shell-env/    login-shell environment capture; resolves pi and oxfmt
crates/storage/      draft directories, meta.json, atomic writes, autosave
```

Library crates go under `crates/<name>` and are added when a subsystem has
its own implementation and tests, such as draft storage or the pi RPC client.
UI code stays in `crates/draftmux` until a split is useful.

### Bumping gpui

`gpui` and `gpui_platform` come from Zed's repository, pinned to one commit in
`[workspace.dependencies]` of the root `Cargo.toml`. gpui has no stable API, so
bump it deliberately:

1. Set the same new `rev` on both `gpui` and `gpui_platform`.
2. Match `rust-toolchain.toml` to Zed's at that rev.
3. Compare Zed's root `[patch.crates-io]` with ours and mirror entries for
   crates in the macOS graph
   (`cargo tree --target aarch64-apple-darwin -i <crate>`).
4. Run `cargo update -p gpui -p gpui_platform`, build, fix API changes, and
   check the window chrome with `cargo run`.
5. Update the Zed rev and line references cited in `docs/design.md`.
