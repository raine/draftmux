# Draftmux

Draftmux is a macOS app for drafting GitHub, Slack and plain-text email
messages with an agent. Hand off from a pi or Claude Code session, polish the
message next to a GitHub, Slack or email preview with an embedded pi agent and
steer-style selection actions, then copy it out or let the original agent read
it back.

Status: early development. See [docs/design.md](docs/design.md) for the design and
[docs/prototype/](docs/prototype/) for HTML prototypes of the main window and
settings.

## Install

Requires macOS with full Xcode installed and the Rust toolchain (see
Development).

```sh
scripts/install
```

This builds `Draftmux.app`, installs it in `~/Applications`, and links
`~/.local/bin/draftmux` to the binary inside the app, which is both the app
and the `draftmux` CLI. Make sure `~/.local/bin` is on your `PATH`. Use
`--app-dir` and `--bin-dir` to install elsewhere. The script only replaces a
previous Draftmux install, never other files. Run it again to update, and
after moving the app, since the link points into it. To uninstall, delete
`Draftmux.app` and the `draftmux` link. Drafts stay in
`~/Library/Application Support/Draftmux/`.

`scripts/bundle` builds the app without installing it and prints its path
(`target/release/Draftmux.app`). The app is ad-hoc signed for running locally,
not notarized for distribution.

### Draft skill

Agents hand messages off to Draftmux with the `draft` skill in
[skills/draft/](skills/draft/SKILL.md): it opens the conversation in a new
draft with `draftmux open`, and reads the polished draft back with
`draftmux show`. Link it into `~/.claude/skills`, after checking that
`~/.claude/skills/draft` doesn't already exist:

```sh
ls -la ~/.claude/skills/draft   # should report "No such file or directory"
mkdir -p ~/.claude/skills
ln -s "$PWD/skills/draft" ~/.claude/skills/draft
```

Claude Code loads it from there. pi doesn't read `~/.claude/skills` by default:
add it to `"skills"` in each pi profile's `settings.json`
(`~/.pi/agent/settings.json`), or link the skill into `~/.agents/skills/draft`
instead, which every profile loads. Don't do both, or pi sees two `draft`
skills.

```json
{ "skills": ["~/.claude/skills"] }
```

In pi, the skill finds the session file through `$PI_SESSION_FILE`, which pi's
bash tool sets. pi extensions that replace the bash tool must pass the tool's
context argument through to the built-in tool's `execute`, or the variable is
missing and the handoff fails. The pi extension Draftmux runs itself is
bundled with the app.

## Development

Requires macOS with full Xcode installed (gpui compiles its Metal shaders at
build time). The Rust toolchain is pinned in `rust-toolchain.toml`.

```sh
cargo run
```

Tests that need real external tools, such as oxfmt, are ignored by default.
Run them with `cargo test -- --ignored`.

### Layout

```text
Cargo.toml           workspace manifest; shared dependencies and the gpui pin
rust-toolchain.toml  toolchain matching Zed at the pinned gpui rev
crates/claude-convert/ Claude Code transcript → pi session conversion (no gpui)
crates/draftmux/     the app binary: startup, window and UI
crates/markup/       message block tree, its parsers and Slack rich text (no gpui)
crates/pi-profiles/  pi profile detection, defaults and model lists (no gpui)
crates/pi-rpc/       pi RPC client, per-draft pi process manager, handoff fork (no gpui)
crates/shell-env/    login-shell environment capture; resolves pi and oxfmt
crates/storage/      draft directories, meta.json, atomic writes, autosave
pi-extension/        pi extension the app passes to pi; bundled as a resource
assets/              app icon master (1024 px)
scripts/             bundle and install scripts
skills/draft/        the `draft` agent skill for Claude Code and pi
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
