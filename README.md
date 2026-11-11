# Draftmux

Polish the messages your coding agent writes before they go out.

Hand a PR description, issue, Slack message or email off from Claude Code or
pi. Draftmux opens it in its own window, where an agent with your conversation
as context writes the first draft. Refine it in chat or by hand beside a
preview of where it's going, then tell your agent it's done and it uses the
text as written.

<!-- Demo video: paste the GitHub user-attachments URL on its own line here. -->

Status: early release; expect rough edges.

## Features

- **Hand-off from your session.** `/draftmux PR description` forks the
  conversation into a [pi](https://github.com/badlogic/pi-mono) agent, which
  writes the first draft from what you've already discussed.
- **Chat with the draft's agent.** Ask for changes in plain words. It runs with
  your pi profile, so your skills are available.
- **Edit beside a preview.** Write Markdown and see it as GitHub, Slack, rich
  text or plain text.
- **Selection actions.** Select text for alternatives, Shorter or Tighten, or
  your own actions. With text selected, a chat message changes only that part.
- **Copy or hand back.** Copy formatted for the target with `⌘⇧C`, or tell your
  agent "done, open the PR".

## Install

### macOS

Requires an Apple silicon Mac with macOS 11 or later.

```sh
brew install --cask raine/draftmux/draftmux
```

This installs `Draftmux.app` and the `draftmux` command. Draftmux keeps itself
up to date.

### Linux

Runs on x86_64 and arm64 with glibc 2.35 or later (Ubuntu 22.04, Debian 12,
Fedora 36 and newer), on Wayland or X11. Download the archive for your
architecture from [Releases](https://github.com/raine/draftmux/releases), then:

```sh
tar -xzf Draftmux-*-linux-"$(uname -m)".tar.gz
draftmux-linux/install
```

This links `~/.local/bin/draftmux`; make sure that directory is on your PATH.
Shortcuts use Ctrl where macOS uses Cmd.

### pi

The agent features run [pi](https://github.com/badlogic/pi-mono):

```sh
npm install -g @earendil-works/pi-coding-agent
```

Without pi, editing, previews and copying still work. If
[oxfmt](https://oxc.rs) is installed, Draftmux uses it to format GitHub and
rich-text drafts.

## Quick start

1. Install the agent skill for Claude Code and pi:

   ```sh
   draftmux skill install
   ```

2. In an agent session, hand the message off: `/draftmux PR description` in
   Claude Code, or `/skill:draftmux PR description` in pi.

3. Polish the draft in Draftmux.

4. Back in the same session, say what to do with it, for example
   `done, open the PR`. From another session, run
   `/draftmux done, open the PR`.

## Configuration

Settings opens with `⌘,` and saves plain JSON to `~/.config/draftmux`, which
you can also edit by hand. Changes apply without a restart. To add your own
selection actions:

```json
"selection_actions": [
  { "name": "More formal", "prompt": "Rewrite in a more formal tone." }
]
```

A draft can start with YAML frontmatter for fields kept apart from the body,
such as `title` for PRs and issues or `subject`, `to` and `cc` for emails.

## Development

Needs the Rust toolchain in `rust-toolchain.toml`, plus full Xcode on macOS.

```sh
cargo run -- app
```

This uses your real drafts. On macOS, `just dev-app` builds and opens a
separate `Draftmux Dev.app` with its own data directory.

## License

MIT. See [LICENSE](LICENSE).
