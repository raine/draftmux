# Draftmux

Draftmux is a macOS and Linux app for drafting GitHub, Slack, rich-text and plain-text
messages with an agent. Hand off from a pi or Claude Code session, polish the
message next to a preview of where it goes with an embedded pi agent and
steer-style selection actions, then copy it out or let the original agent read
it back.

Status: early release; expect rough edges.

## Install

### macOS

Requires an Apple silicon Mac with macOS 11 or later.

```sh
brew install --cask raine/draftmux/draftmux
```

The cask installs `Draftmux.app` and links the `draftmux` command, which
agents use to hand drafts off. Without Homebrew, download the disk image from
[Releases](https://github.com/raine/draftmux/releases), drag Draftmux to
Applications, and link the command yourself:

```sh
ln -s /Applications/Draftmux.app/Contents/MacOS/draftmux ~/.local/bin/draftmux
```

Draftmux keeps itself up to date (see [Updates](#updates)). Drafts are stored
in `~/Library/Application Support/Draftmux/`.

### Linux

Wayland and X11 are supported on x86_64 and arm64 distributions with glibc
2.35 or later (Ubuntu 22.04, Debian 12, Fedora 36 and newer). Download
`Draftmux-<version>-linux-x86_64.tar.gz` or
`Draftmux-<version>-linux-aarch64.tar.gz` from
[Releases](https://github.com/raine/draftmux/releases), then:

```sh
tar -xzf Draftmux-*-linux-"$(uname -m)".tar.gz
draftmux-linux/install
```

This installs the app and its pi extension in `~/.local/lib/draftmux` and
links `~/.local/bin/draftmux`. The desktop entry and icon go under
`${XDG_DATA_HOME:-~/.local/share}`. Add `~/.local/bin` to PATH for CLI and
agent use. To update, install a newer release the same way; the installer
refuses to replace an unrelated command or unmarked app tree.
`DRAFTMUX_INSTALL_PREFIX` overrides `~/.local`.

To build from source instead, install the Rust toolchain from
`rust-toolchain.toml`, just, a C/C++ toolchain and pkg-config. On Debian 12 /
Ubuntu, the build libraries are:

```sh
sudo apt install build-essential pkg-config cmake clang libxkbcommon-dev \
  libxkbcommon-x11-dev libwayland-dev libx11-dev libx11-xcb-dev libxcb1-dev \
  libfontconfig-dev libfreetype-dev libvulkan-dev libzstd-dev
just linux-install
```

This builds a debug binary with two cargo jobs and installs it the same way.
`just linux-app` produces `target/draftmux-linux-<architecture>.tar.gz`
without installing it. The binary finds resources relative to itself,
including when launched through the command symlink.

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

## Usage

The chat and selection actions run
[pi](https://github.com/badlogic/pi-mono)
(`npm install -g @earendil-works/pi-coding-agent`), which Draftmux finds on
your login shell's PATH. Without pi, editing, previews and copying still work.
Formatting uses [oxfmt](https://oxc.rs) when it is installed, for GitHub and
rich-text drafts.

Slack drafts are written in Markdown, not Slack's mrkdwn, with Slack's rules
on top: every line break shows, and headings and tables paste as a bold line
and a code block.

Toggle the drafts sidebar with the toolbar’s sidebar button or `Cmd+\`
(`Ctrl+\` on Linux). Draftmux remembers its visibility across restarts.

To hand a message off from Claude Code or pi, install the agent skill and
invoke it from the agent's session, for example `/draftmux PR description`:

```sh
draftmux skill install
```

See [Draftmux skill](#draftmux-skill) for the full handoff and read-back flow.

### Configuration

Everything the Settings window changes is saved in plain JSON files, which you
can also edit by hand: `settings.json` holds the defaults, directory rules and
selection actions, and `profiles.json` the pi profile folders added in
Draftmux. They live in `~/.config/draftmux` (or `$XDG_CONFIG_HOME/draftmux`)
on macOS and Linux; set `DRAFTMUX_CONFIG_DIR` to use another directory.
Settings shows the path under Files. Older versions kept these files in the
data directory beside `drafts/`, and Draftmux moves them on first launch.

Draftmux applies a saved edit within a second, without a restart. If the file
doesn't parse, Settings shows the error and the last valid settings stay in
use until the file is fixed. New drafts pick up changed settings; existing
drafts keep the settings they were created with. Selection actions and the
alternatives count apply immediately to all drafts.

The app-wide `selection_actions` key in `settings.json` is an ordered list of
objects with `name` and `prompt` strings, for example:

```json
"selection_actions": [
  { "name": "More formal", "prompt": "Rewrite in a more formal tone." }
]
```

Omit the key to use Shorter and Tighten, or set it to `[]` for no custom
actions. Invalid entries, including blank names or prompts, are skipped.
Alternatives remains available. Manage the list in Settings → Selection
actions. For anything else, keep the text selected and ask the draft's agent in
the chat: the selection is attached to your next message.

### Updates

A release installed from the disk image or Homebrew checks GitHub for a
newer release once a day. It downloads the new version in the background,
verifies its checksum and Developer ID signature, and replaces
`Draftmux.app` in place. A Restart to Update button then appears in the
toolbar. If Draftmux can't write to the folder holding the app, the toolbar
shows Update Available instead, which opens the release page. Draftmux >
Check for Updates… checks right away and reports the result.

Turn automatic checks off under Settings → General → Updates
(`"check_for_updates": false` in `settings.json`), or set
`DRAFTMUX_NO_UPDATE_CHECK=1`. Check for Updates still works either way.
Linux installs and builds made from source never check on their own and
don't update themselves; Check for Updates tells them a newer release
exists. Checks contact `api.github.com` and GitHub's download hosts and send
nothing but the request.

### Draft fields

Drafts can start with YAML frontmatter for fields separate from the message:

```markdown
---
title: Add Aven to the app showcase
---

Adds [Aven](https://github.com/raine/aven) to the app showcase.
```

Use `title` for PRs and issues, or `subject`, `to` and `cc` for emails.
Other keys are allowed. Values are strings; use comma-separated text for
recipients or labels rather than YAML lists. The preview shows fields in file
order, with a copy button for each. Copy (⌘⇧C) in every format excludes the
frontmatter. Edit the fields as raw text in the editor.

The sidebar and header use `title`, then `subject`, then the first body line.
An explicitly stored metadata title takes precedence. Invalid frontmatter stays
part of the body so no text is lost.

`draftmux show --json` returns an ordered `fields` map and the body without
frontmatter. Plain `show` and the file returned by `show --path` retain the
frontmatter. The JSON top-level `title` remains the stored metadata title. `draftmux list`
prints the sidebar's title; its JSON keeps the stored one in `title` and adds
the sidebar's as `display_title`.

### Draftmux skill

Agents hand messages off to Draftmux with the `draftmux` skill: it opens the
conversation in a new draft with `draftmux open`, and reads the polished draft
back with `draftmux show`. It only runs when you invoke it: `/draftmux PR
description` in Claude Code or `/skill:draftmux` in pi to hand off. To read
the draft back, tell the agent in the same conversation that it's done and what
to do with it, for example "done, open the PR". In a conversation that didn't
hand the draft off, invoke the skill again: `/draftmux done, open the PR`. Install it with:

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
cargo run -- app
```

A bare `draftmux` prints help; `draftmux app` runs the app in the
current process. `cargo run -- app` uses your real drafts. On Linux,
use `DRAFTMUX_DATA_DIR="$(mktemp -d)" cargo run -j 2 -p draftmux -- app` for
scratch data.
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

### Typing performance

Build with `cargo build -j 2 -p draftmux --bin draftmux`, then run
`tools/profile-typing small` or `tools/profile-typing large`. These open isolated
992-byte and 98 KB drafts; they do not use your drafts or start a pi session.
On macOS the tool launches a temporary app bundle and prints the timing log's
path. Elsewhere timings go to stderr. Quit the fixture app when finished.

Type the same text at the same pace and window size for each comparison.
`DRAFTMUX_TIMING=1` enables timings in milliseconds for input handling,
highlighting, edit subscribers, preview parsing, preview frames and editor
layout. `input-to-editor-paint` starts in the native text input handler and ends
at editor paint: it **does not** include time queued in the OS, GPU presentation
or display scanout. Check `preview-frame` as well: a slow preview frame can
still delay subsequent input delivery. Compare the same build profile; debug
numbers are not release latency estimates.

For component-only CPU measurements, run:

```sh
cargo test -j 2 -p draftmux --bin draftmux performance::typing -- --ignored --nocapture
```

### Release

`just release-macos` builds a release on an Apple silicon Mac into
`target/macos-release/<version>/`: `Draftmux.app` signed with a Developer ID
certificate, notarized and stapled, inside a disk image that is itself
signed, notarized and stapled, plus the image's `.sha256`. The stapled app
is also archived as `Draftmux-<version>-arm64-app.tar.gz` with its own
`.sha256`, which automatic updates install. It checks credentials before
building. It submits the app and disk image to Apple's notary service but
publishes nothing. Publish all four files on a `v<version>` GitHub release
of `raine/draftmux`. Updates read the latest non-draft, non-prerelease
release; one without the app archive fails the check. It needs:

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

`scripts/release-linux` builds an optimized Linux release into
`target/linux-release/<version>/`: the app tree that `just linux-app`
packages, as `Draftmux-<version>-linux-<architecture>.tar.gz` with its
`.sha256`. The binary needs a glibc at least as new as the build machine's.

The Release workflow runs `scripts/release-macos` on GitHub Actions with the
variables above as repository secrets, and `scripts/release-linux` on Ubuntu
22.04. Pushing a `vVERSION` tag that matches the version in
`crates/draftmux/Cargo.toml` publishes the disk image, the app archive, the
Linux archive and their checksums as a GitHub release, with that version's
`CHANGELOG.md` section as its notes, then writes the cask to
`Casks/draftmux.rb` in
[raine/homebrew-draftmux](https://github.com/raine/homebrew-draftmux), using
the `RELEASE_TOKEN` secret, a token that can push to that repository. Running
the workflow by hand builds, signs and notarizes the same files as workflow
artifacts without publishing anything.

`just release` cuts a release with `cargo-release` from
[rust-release-tools](https://github.com/raine/rust-release-tools): it bumps
the patch version (or `minor`, `major`, or `current` to release the version
as is) in `crates/draftmux/Cargo.toml`, writes the `CHANGELOG.md` entry and
opens it for review, then commits, tags and pushes, which starts the Release
workflow. Pass `--skip-changelog` to release the `## Unreleased` section as
written.

The bundle ID is `CFBundleIdentifier` in `macos/Info.plist`, which every
script reads; the updater's `BUNDLE_ID` in `crates/draftmux/src/update.rs`
must match it. It is part of the signature's designated requirement, so
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
   check the window chrome with `cargo run -- app`.

## License

MIT. See [LICENSE](LICENSE).
