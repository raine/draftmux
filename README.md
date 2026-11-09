# Draftmux

Draftmux is a macOS app for drafting GitHub, Slack and plain-text email
messages with an agent. Hand off from a pi or Claude Code session, polish the
message next to a GitHub, Slack or email preview with an embedded pi agent and
steer-style selection actions, then copy it out or let the original agent read
it back.

Status: early development.

## Install

Requires macOS with full Xcode installed, the Rust toolchain (see
Development) and [just](https://github.com/casey/just).

```sh
just install-app
```

This builds `Draftmux.app`, installs it in `~/Applications`, and links
`~/.local/bin/draftmux` to the binary inside the app, which is both the app
and the `draftmux` CLI. Make sure `~/.local/bin` is on your `PATH`. Use
`--app-dir` and `--bin-dir` to install elsewhere, and `--app PATH` to install
an already built `Draftmux.app`. It only replaces a previous Draftmux
install, never other files. Run it again to update, and after moving the app,
since the link points into it. To uninstall, delete `Draftmux.app` and the
`draftmux` link. Drafts stay in `~/Library/Application Support/Draftmux/`.

`just app` builds the app without installing it and prints its path
(`target/release/Draftmux.app`); `just app-debug` builds
`target/debug/Draftmux.app`. Both are ad-hoc signed for running locally, not
notarized for distribution.

### Draftmux skill

Agents hand messages off to Draftmux with the `draftmux` skill in
[skills/draftmux/](skills/draftmux/SKILL.md): it opens the conversation in a
new draft with `draftmux open`, and reads the polished draft back with
`draftmux show`. It only runs when you invoke it: `/draftmux PR description`
in Claude Code or `/skill:draftmux` in pi to hand off, and `/draftmux done`
plus what to do with it (for example "open the PR") to read the draft back.
Link it into `~/.claude/skills`, after checking that
`~/.claude/skills/draftmux` doesn't already exist:

```sh
ls -la ~/.claude/skills/draftmux   # should report "No such file or directory"
mkdir -p ~/.claude/skills
ln -s "$PWD/skills/draftmux" ~/.claude/skills/draftmux
```

Claude Code loads it from there. pi doesn't read `~/.claude/skills` by default:
add it to `"skills"` in each pi profile's `settings.json`
(`~/.pi/agent/settings.json`), or link the skill into
`~/.agents/skills/draftmux` instead, which every profile loads. Don't do both,
or pi sees two `draftmux` skills.

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

`cargo run` uses your real drafts. To try a change without touching them, run
`just dev-app`: it builds a debug `Draftmux Dev.app` and opens it with its own
data directory under `/tmp/dmx.*/data` (set through `DRAFTMUX_DATA_DIR`),
beside the installed app. Your pi profiles and shell environment stay the
same. It prints the command to drive it from the CLI.

Tests that need real external tools, such as oxfmt, are ignored by default.
Run them with `cargo test -- --ignored`.

`just test-scripts` tests the build, install and release scripts with stub
binaries, without cargo or Apple credentials; `just shellcheck` lints them.

### Release

`just release-macos` builds a release on an Apple silicon Mac into
`target/macos-release/<version>/`: `Draftmux.app` signed with a Developer ID
certificate, notarized and stapled, inside a disk image that is itself
signed, notarized and stapled, plus the image's `.sha256`. It checks
credentials before building and uploads or publishes nothing. It needs:

| Variable                           | Value                                              |
| ---------------------------------- | -------------------------------------------------- |
| `APPLE_DEVELOPER_ID_CERT_BASE64`   | the Developer ID Application `.p12`, base64        |
| `APPLE_DEVELOPER_ID_CERT_PASSWORD` | the `.p12` password                                |
| `APPLE_DEVELOPER_ID_IDENTITY`      | `Developer ID Application: NAME (TEAM)`            |
| `APPLE_TEAM_ID`                    | the 10-character team ID the signature must carry  |
| `ASC_KEY_ID`, `ASC_ISSUER_ID`      | App Store Connect API key ID and issuer ID         |
| `ASC_KEY_P8`                       | the API key's `.p8` contents                       |

The steps are separate scripts in `scripts/`: `sign-macos-release`,
`notarize-macos-release` (which staples through `stapler`),
`verify-macos-release`, `create-macos-dmg` and `check-macos-dmg`. To check a
signed build's verification, run `scripts/test-verify-macos-release` with the
signed app or disk image.

Print the Homebrew cask for a release with
`just homebrew-cask VERSION CHECKSUM_FILE URL HOMEPAGE`, where `URL` is where
the disk image is published and may use `#{version}`.

The bundle ID is `CFBundleIdentifier` in `macos/Info.plist`, which every
script reads. It is part of the signature's designated requirement, so
changing it after a release loses macOS permissions and preferences tied to
the old one. The disk image window comes from `macos/dmg/`; after editing
`background.svg`, regenerate it with `just dmg-layout` (needs `rsvg-convert`
and `uv`) and commit the output.

### Layout

```text
Cargo.toml           workspace manifest; shared dependencies and the gpui pin
rust-toolchain.toml  toolchain matching Zed at the pinned gpui rev
crates/claude-convert/ Claude Code transcript → pi session conversion (no gpui)
crates/draftmux/     the app binary: startup, window and UI
crates/markup/       message block tree, its parsers and Slack rich text (no gpui)
crates/pi-profiles/  pi profile detection, defaults and model lists (no gpui)
crates/pi-rpc/       pi RPC client, per-draft pi process manager, handoff fork (no gpui)
crates/settings/     settings.json, directory rules and their resolution (no gpui)
crates/shell-env/    login-shell environment capture; resolves pi and oxfmt
crates/storage/      draft directories, meta.json, atomic writes, autosave
pi-extension/        pi extension the app passes to pi; bundled as a resource
macos/               Info.plist, app icon master (1024 px) and disk image layout
scripts/             build, install and release scripts and their tests
skills/draftmux/     the `draftmux` agent skill for Claude Code and pi
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
