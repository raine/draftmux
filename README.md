# Draftmux

Polish the text your coding agent writes before it goes out.

Your agent knows the context, so it should be the one writing the PR description,
the issue or the Slack update. But refining that message inside an agent
harness sucks.

Draftmux is an editor built for iterating on a message with an agent. Hand the
message off from Claude Code or pi, and it opens next to a live preview of how
it will look on GitHub, in Slack or wherever it's going. Edit it by hand,
rewrite a selection with one click, or ask an agent forked from your session
for changes in chat. When it's ready, tell your original agent it's done,
and it uses the text exactly as you left it.

https://github.com/user-attachments/assets/c01af06e-e89b-432c-b077-e68d4e0b0148

Status: early release; expect rough edges.

## Features

- **Hand-off from your session.** `/draftmux Slack update for the team` forks
  the conversation into a new pi or Claude Code session, which writes the first
  draft from what you've already discussed.
- **Chat with the draft's agent.** Ask for changes in plain words. On pi it runs
  with your profile, so your skills are available.
- **Edit beside a preview.** Write Markdown and see it as GitHub, Slack, rich
  text or plain text. Click a task list checkbox in the GitHub preview to tick
  it off in the Markdown.
- **Selection actions.** Select text to get alternatives, Shorten or Tighten, or
  your own actions. `Esc` leaves the options for later while you keep writing:
  the passage stays marked, and its badge in the margin, `⌥↓` or `⌥↑` bring
  them back. Several can run at once; `⌘⌫` discards the ones shown, and
  editing the passage drops them. With text selected, a chat message changes
  only that part. Hold `⌘` while dragging to select more passages; a chat
  message then edits each of them.
- **Continue the sentence.** `⌘↩` asks the draft's agent to finish the sentence
  at the cursor and shows it as faint text: `⇥` takes it, `⌥→` takes a word,
  typing it keeps it going, and `Esc` drops it. `⌘⇧↩` offers several
  continuations to pick from.
- **Copy or hand back.** Copy formatted for the target with `⌘⇧C`, or tell your
  agent "done, file the issue".
- **Edit files.** `draftmux README.md` opens a file as a draft, with its own
  agent session in the file's directory. `⌘S` saves it back to the file.

`⌘/` or the `?` at the bottom of the sidebar opens Help, which lists the
keyboard shortcuts.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/raine/draftmux/main/install.sh | sh
```

This installs the latest release and links the `draftmux` command in
`~/.local/bin`; make sure that directory is on your PATH. On Linux, run it
again to update. On Windows, use PowerShell instead:

```powershell
irm https://raw.githubusercontent.com/raine/draftmux/main/install.ps1 | iex
```

### macOS

Requires an Apple silicon Mac with macOS 11 or later. The script installs
`Draftmux.app` in `/Applications`. With Homebrew, instead:

```sh
brew install --cask raine/draftmux/draftmux
```

Draftmux keeps itself up to date either way.

To install from the disk image instead, download the `.dmg` from the
[latest release](https://github.com/raine/draftmux/releases/latest) and drag
Draftmux into Applications. The disk image doesn't install the `draftmux`
command that the Pi and Claude Code skills run: open Draftmux from
Applications and choose **Draftmux › Install Command Line Tool…**, which the
first launch also offers. It links the command into `/usr/local/bin`, which
is on the PATH by default, and asks for an administrator password when that
folder isn't writable.

### Linux

Runs on x86_64 and arm64 with glibc 2.35 or later, such as Ubuntu 22.04 or
Debian 12 and newer. The script installs the app in `~/.local/lib/draftmux`,
and Draftmux keeps itself up to date there. Shortcuts use Ctrl where macOS
uses Cmd.

### Windows

Runs on x64 Windows 10 version 1803 or later. The script installs the app in
`%LOCALAPPDATA%\Programs\Draftmux`, adds the `draftmux` command to your PATH
and creates a Start Menu shortcut. Draftmux tells you when a new version is
out; run the script again to update. Shortcuts use Ctrl where macOS uses Cmd.

The Windows build isn't code signed, so SmartScreen may warn about a
`draftmux.exe` unpacked from a downloaded zip; the script's install doesn't
get that warning. Claude Code drafts need Claude Code's native installer:
Draftmux doesn't run the `claude.cmd` that npm installs. Draftmux reads API
keys from the environment it starts in rather than from your PowerShell
profile, and finds tools on your saved PATH.

Run the script in your own PowerShell window rather than in an agent's shell.
Agents in packaged apps such as Claude's desktop app run in shells whose
writes under `AppData` Windows keeps private to that app, so an install from
there isn't visible outside it. Restart apps you run agents in, such as
Claude's desktop app or your terminal, so their shells find `draftmux`.

### Agents

Drafts are written by [pi](https://github.com/earendil-works/pi) or Claude Code
2.1.293 or later.

Editing, previews, copying and Markdown formatting of GitHub and rich-text
drafts work without either.

## Quick start

1. Install the agent skill for Claude Code and pi:

   ```sh
   draftmux skill install
   ```

2. In an agent session, hand the message off: `/draftmux PR description` in
   Claude Code, or `/skill:draftmux PR description` in pi.

3. Polish the draft in Draftmux.

4. Back in the same session, say what to do with it, for example
   `done, open the PR`. From another session, run `/draftmux done, open the PR`.

## Choosing the agent

A draft runs on the agent of the session it came from: pi sessions continue on
pi and Claude Code sessions on Claude Code. To draft Claude Code sessions on pi
instead, converted from the transcript, set Backend to pi in Settings, globally
or for one directory. For a single draft, ask the skill
(`/draftmux PR description, on pi`) or pass `--backend pi` or
`--backend claude` to `draftmux open`. A draft stays on the agent it was
created with.

## How it works with pi

- Each draft gets its own pi session, forked from your pi session or converted
  from a Claude Code transcript. Your original session is untouched.
- Draftmux drives pi through its RPC mode (`pi --mode rpc`), one process per
  draft, with your own config (`~/.pi/agent` or another profile), so your
  logins, models and skills apply.
- A bundled extension, passed with `-e`, limits edits to the draft file and
  tells the agent the target. Nothing is installed into your pi.
- Selection actions run in a throwaway fork of the draft's session with tools
  blocked.
- The original agent reads the finished draft with `draftmux show`.

## How it works with Claude Code

- The draft continues your conversation in its own Claude Code session. Your
  original session is untouched.
- Claude Code may read files but edit only the draft. It runs without your
  Claude Code hooks, MCP servers or other settings, except the `env` and
  `apiKeyHelper` in your user `settings.json`, so a gateway or API key you log
  in with keeps working. It keeps the `CLAUDE.md` files and `.claude/rules`
  your session loaded, read fresh each time the draft's agent starts.
  `CLAUDE.local.md` isn't loaded.
- New drafts use the session's model unless Settings sets a Claude Code
  drafting model such as `sonnet`. The model chip changes a draft's model, and
  the draft keeps the choice.
- Selection actions run in a throwaway copy of the conversation with no tools.
  They use the draft's model unless Settings sets a Claude Code selection
  model.
- If Draftmux can't run Claude Code, `open` reports an error instead of
  switching to pi.

## Configuration

Settings opens with `⌘,` and saves plain JSON to `~/.config/draftmux`, which you
can also edit by hand. Changes apply without a restart. To add your own
selection actions:

```json
"selection_actions": [
  { "name": "More formal", "prompt": "Rewrite in a more formal tone." }
]
```

A draft can start with YAML frontmatter for fields kept apart from the body,
such as `title` for PRs and issues or `subject`, `to` and `cc` for emails.

## Troubleshooting

Errors and crash reports go to `draftmux.log` in
`~/Library/Application Support/Draftmux` on macOS, `~/.local/share/Draftmux`
on Linux and `~\.local\share\Draftmux` on Windows. Once the log passes 1 MB,
the app moves it to `draftmux.log.1` on its next start.

`draftmux` commands start the app and reach it through a local socket, so
they need to run outside an agent's shell sandbox. If a command reports that
it can't connect to `draftmux.sock`, quit Draftmux and run the command again
from a shell without a sandbox.

## Development

Needs the Rust toolchain in `rust-toolchain.toml`, plus full Xcode on macOS.

```sh
cargo run -- app
```

This uses your real drafts. On macOS, `just dev-app` builds and opens a separate
`Draftmux Dev.app` with its own data directory.

## License

MIT. See [LICENSE](LICENSE).
