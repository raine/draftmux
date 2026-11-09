# Draftmux

Draftmux is a macOS and Linux app for drafting GitHub, Slack, rich-text and plain-text
messages with an agent. Hand off from a pi or Claude Code session, polish the
message next to a preview of where it goes with an embedded pi agent and
steer-style selection actions, then copy it out or let the original agent read
it back.

Status: early development.

## Install

### macOS

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

### Linux

Wayland and X11 are supported on glibc distributions. Install the Rust
toolchain from `rust-toolchain.toml`, just, a C/C++ toolchain and pkg-config.
On Debian 12 / Ubuntu, the build libraries are:

```sh
sudo apt install build-essential pkg-config cmake clang libxkbcommon-dev \
  libxkbcommon-x11-dev libwayland-dev libx11-dev libx11-xcb-dev libxcb1-dev \
  libfontconfig-dev libfreetype-dev libvulkan-dev libzstd-dev
just linux-install
```

This builds a debug binary with two cargo jobs, installs the app and its pi
extension in `~/.local/lib/draftmux`, and links `~/.local/bin/draftmux`.
The desktop entry and icon go under `${XDG_DATA_HOME:-~/.local/share}`.
Add `~/.local/bin` to PATH for CLI and agent use. Run the recipe again to
update; it refuses to replace an unrelated command or unmarked app tree.
`DRAFTMUX_INSTALL_PREFIX` overrides `~/.local`.

`just linux-app` produces `target/draftmux-linux-<architecture>.tar.gz`
without installing it. Extract it and run `draftmux-linux/install` on a
machine with a compatible glibc and architecture. The binary finds resources
relative to itself, including when launched through the command symlink.
These are local builds, not automatically published releases.

Runtime packages on Debian are `libxcb1 libxkbcommon0 libxkbcommon-x11-0
libfreetype6 libvulkan1 libegl1 libwayland-client0 libwayland-egl1 fontconfig
fonts-dejavu-core fonts-liberation`, plus graphics drivers (Mesa:
`mesa-vulkan-drivers libegl-mesa0 libgl1-mesa-dri`). Run in a graphical
session with a session D-Bus bus. pi and oxfmt are optional external tools,
just as on macOS.

Linux shortcuts use Ctrl instead of Cmd: Ctrl+Q quits, Ctrl+, opens Settings,
Ctrl+Shift+C copies for the target, and Ctrl+Shift+F formats. Settings is also
available in the main toolbar. The editor uses Ctrl+Left/Right for words,
Ctrl+Home/End for the document, and Ctrl+A to select all.

Rich text and Slack copy HTML with a plain-text alternative. Wayland rich
copy requires data-control support or a working XWayland clipboard bridge.
When unavailable, Draftmux copies plain text and shows a warning toast.
Client-side window controls appear when the compositor provides none.
Linux previews keep the Apple Mail sizing with Liberation Sans and DejaVu
Sans Mono in place of Helvetica and Menlo.

Drafts live in `${XDG_DATA_HOME:-~/.local/share}/Draftmux`. To uninstall,
remove `~/.local/lib/draftmux`, the `~/.local/bin/draftmux` symlink, and
`io.github.raine.Draftmux.desktop` / `io.github.raine.Draftmux.png` from
the applications and icons directories. Drafts are not removed.

### Configuration

Everything the Settings window changes is saved in plain JSON files, which you
can also edit by hand: `settings.json` holds the defaults, directory rules and
alternatives count, and `profiles.json` the pi profile folders added in
Draftmux. They live in `~/.config/draftmux` (or `$XDG_CONFIG_HOME/draftmux`)
on macOS and Linux; set `DRAFTMUX_CONFIG_DIR` to use another directory.
Settings shows the path under Files. Older versions kept these files in the
drafts directory and Draftmux moves them on first launch.

Draftmux applies a saved edit within a second, without a restart. If the file
doesn't parse, Settings shows the error and the last valid settings stay in
use until the file is fixed. New drafts pick up changed settings; existing
drafts keep the settings they were created with.

### Draftmux skill

Agents hand messages off to Draftmux with the `draftmux` skill: it opens the
conversation in a new draft with `draftmux open`, and reads the polished draft
back with `draftmux show`. It only runs when you invoke it: `/draftmux PR
description` in Claude Code or `/skill:draftmux` in pi to hand off, and
`/draftmux done` plus what to do with it (for example "open the PR") to read
the draft back. Install it with:

```sh
draftmux skill install
```

`draftmux show` finds the draft from the agent's session file, so the agent
doesn't need the ID. When it can't, for example after the session was resumed
under a new file, `draftmux list` prints the drafts, newest first (`--cwd DIR`
for one project's, `--all` to include archived ones, `--json` for scripts).

This writes a small `SKILL.md` to `~/.claude/skills/draftmux` (or
`$CLAUDE_CONFIG_DIR/skills/draftmux`) and links each pi profile's
`skills/draftmux` (`~/.pi/agent`, `~/.pi-*/agent` and profiles added in
Draftmux) to it. The skill only runs `draftmux skill`, which prints the
instructions built into the binary, so updating Draftmux updates the skill
without reinstalling it. pi sees one `draftmux` skill even if a profile also
lists `~/.claude/skills` in its `"skills"` setting, because both paths lead to
the same file. If `~/.agents/skills/draftmux` exists, which every pi profile
loads, install leaves pi alone and links Claude Code's skill to it instead.

Running install again updates the skills it installed. It never replaces
anything else at those paths: it reports the path and skips it. Remove what is
there and run install again to replace it.

In pi, the skill finds the session file through `$PI_SESSION_FILE`, which pi's
bash tool sets. pi extensions that replace the bash tool must pass the tool's
context argument through to the built-in tool's `execute`, or the variable is
missing and the handoff fails. The pi extension Draftmux runs itself is
bundled with the app.

## Development

On macOS, full Xcode is required (gpui compiles Metal shaders at build time).
On Linux, install the build libraries above. The Rust toolchain is pinned
in `rust-toolchain.toml`.

```sh
cargo run
```

`cargo run` uses your real drafts. On Linux, use
`DRAFTMUX_DATA_DIR="$(mktemp -d)" cargo run -j 2 -p draftmux` for scratch data.
On macOS, to try a change without touching them, run
`just dev-app`: it builds a debug `Draftmux Dev.app` and opens it with its own
data directory under `/tmp/dmx.*/data` (set through `DRAFTMUX_DATA_DIR`,
which also holds its settings unless `DRAFTMUX_CONFIG_DIR` is set), beside
the installed app. Your pi profiles and shell environment stay the
same. It prints the command to drive it from the CLI.

Tests that need real external tools, such as pi or oxfmt, are ignored by
default. Run them per package with `cargo test -p PACKAGE -- --ignored`.

`just test-scripts` tests the build, install and release scripts with stub
binaries, without cargo or Apple credentials; `just shellcheck` lints them.

### Release

`just release-macos` builds a release on an Apple silicon Mac into
`target/macos-release/<version>/`: `Draftmux.app` signed with a Developer ID
certificate, notarized and stapled, inside a disk image that is itself
signed, notarized and stapled, plus the image's `.sha256`. It checks
credentials before building. It submits the app and disk image to Apple's
notary service but publishes nothing. It needs:

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
Cargo.toml             workspace manifest; shared dependencies and the gpui pin
rust-toolchain.toml    toolchain matching Zed at the pinned gpui rev
crates/claude-convert/ Claude Code transcript → pi session conversion (no gpui)
crates/draftmux/       the app binary and CLI: startup, windows and app UI
crates/editor/         markdown source editor: wrapping, undo, highlighting
crates/markup/         message block tree, its parsers and Slack rich text (no gpui)
crates/pi-profiles/    pi profile detection, defaults and model lists (no gpui)
crates/pi-rpc/         pi RPC client, per-draft pi process manager, handoff fork (no gpui)
crates/settings/       settings.json, directory rules and their resolution (no gpui)
crates/shell-env/      login-shell environment capture; resolves pi and oxfmt
crates/storage/        draft directories, meta.json, atomic writes, autosave
pi-extension/          pi extension the app passes to pi; bundled as a resource
macos/                 Info.plist, app icon master (1024 px) and disk image layout
scripts/               build, install and release scripts and their tests
skills/draftmux/       agent skill stub; its instructions are crates/draftmux/src/skill.md
```

Library crates go under `crates/<name>` and are added when a subsystem has
its own implementation and tests, such as draft storage or the pi RPC client.

### Bumping gpui

`gpui` and `gpui_platform` come from Zed's repository, pinned to one commit in
`[workspace.dependencies]` of the root `Cargo.toml`. gpui has no stable API, so
bump it deliberately:

1. Set the same new `rev` on both `gpui` and `gpui_platform`.
2. Match `rust-toolchain.toml` to Zed's at that rev.
3. Compare Zed's root `[patch.crates-io]` with ours and mirror entries for
   crates in the enabled platform graphs
   (`cargo tree --target aarch64-apple-darwin -i <crate>`).
4. Run `cargo update -p gpui -p gpui_platform`, build, fix API changes, and
   check the window chrome with `cargo run`.
