# Changelog

## Unreleased

- New: Start a draft from scratch in the app with New draft (`⌘N`, or the `+`
  beside Drafts in the sidebar). It opens empty with an agent of its own, ready
  for a Slack update or an email; copy it out when it's done. A new draft left
  empty is deleted rather than kept.
- New: A Claude Code draft's agent can run shell commands to check facts, as
  pi's already could. Its edits still only reach the draft, and commands can't
  run in the background.

## v0.6.0 (2026-11-22)

- New: Compact a draft's conversation so replies and selection actions come
  faster. The size chip at the top of the chat opens a Compact button, and
  Draft › Compact Session (`⌥⌘K`) does the same.
- New: Hand off a summary instead of the whole session. The draft starts a fresh
  session from a summary the handing-off agent writes, which can still read the
  original session for details. Ask the skill for it, or set Hand-off context to
  Summary in Settings.
- New: Drafts leave out images the source agent read, such as screenshots, so
  replies and selection actions don't upload them with every request. The
  agent can read one again if the message needs it; images you attached stay.
- Fix: Selection actions on pi drafts using OpenAI models read the prompt cache
  instead of sending the whole conversation uncached on every call.
- Fix: The first selection action after a chat reply on a pi draft using an
  Anthropic model reads the conversation from the prompt cache instead of
  writing all of it again.
- Fix: Running another selection action from the same popover, or continuing a
  sentence after an earlier action, no longer takes a few seconds longer than
  the first.

## v0.5.0 (2026-11-20)

- New: Alternatives and selection actions keep running when you close the
  popover. `Esc` leaves them for later: the passage stays marked, and its badge
  in the margin, a click on it, `⌥↓` or `⌥↑` brings the options back. Seen
  options go when closed unless `⇥` keeps them; `⌘⌫` or editing the passage
  discards them.
- New: Draftmux › Install Command Line Tool… puts the `draftmux` command on
  your PATH when you installed from the disk image, so the Pi and Claude Code
  skills can run it. The first launch from Applications offers it.
- Fix: Claude Code drafts no longer stop following the session's `CLAUDE.md`
  files and `.claude/rules`.
- Fix: Claude Code drafts no longer fail with "Not logged in" when Claude Code
  signs in through its `settings.json`, for example through a company API
  gateway with an `apiKeyHelper`.
- Fix: In Claude Code drafts, alternatives, selection actions and continuations
  now use the effort chosen in the chat, unless a separate selection model is
  set.
- Fix: A continuation typed right after an opening `**`, `_`, backtick, quote or
  bracket starts right after it instead of after a space
- Fix: Pressing `⌘/` in the Help window, `⌘,` in Settings or choosing About
  again while it's in front no longer opens a second copy of the window.
- UX: Alternatives, selection actions and continuations answer faster:
  - Pick an alternative as soon as it appears.
  - The agent starts loading when the popover opens.
  - On long conversations, later requests reuse the model provider's cache
    (Claude Code drafts, and pi drafts on Claude models).
- UX: Alternatives are more varied. The first is a close rewording, and the rest
  try different ways of making the point or say something else there.
- UX: Alternatives and action results show formatted text instead of Markdown
  markers, and hard-wrapped text reads as one paragraph.

## v0.4.0 (2026-11-19)

- New: `⌘↩` continues the sentence at the cursor, shown as faint text you take
  with `⇥`, word by word with `⌥→`, or by typing it. `⌘⇧↩` offers several
  continuations to pick from instead.
- New: Help lists the keyboard shortcuts. Open it with `⌘/`, the `?` at the
  bottom of the sidebar, or the Help menu.
- New: Change a draft's thinking level by clicking it next to the model name
  in the chat.
- New: Click a task list checkbox in the GitHub preview to check or uncheck it
  in the Markdown.
- UX: While the agent edits a draft, the editor fades instead of turning gray,
  so the Markdown highlighting stays visible.

## v0.3.2 (2026-11-17)

- New: An About window shows the version and links to the website and source.
- New: Drag the divider between the editor and preview to resize them.
- New: Settings shows when Draftmux last checked for updates and whether one is
  downloading or ready to install.
- Fix: Draftmux checks for updates every time it starts, then once a day.
- Fix: Check for Updates in the app menu shows the release notes once the update
  is installed, instead of a message that vanished too quickly to read.
- Fix: The Draftmux skill tells agents to write a Slack message handed off as a
  file in Markdown, so bold titles no longer show as italic.

Thanks [@erikjuhani](https://github.com/erikjuhani) for the feedback!

## v0.3.1 (2026-11-16)

- Fix: Switching to another app no longer closes the selection popover or
  drops its suggestions.
- Fix: Clicking or moving the cursor away from a pending accent (a dead key
  such as `` ` ``) finishes it, so the next keys type at the new cursor.
- Fix: Swiping sideways across a wide table or code block in the preview no
  longer scrolls the page up or down.

Thanks [@erikjuhani](https://github.com/erikjuhani) and
[@leodnlv](https://github.com/leodnlv)!

## v0.3.0 (2026-11-15)

- New: Draftmux runs on Windows. See the README for installation.
- Fix: Processes that pi starts no longer keep running after pi exits.

## v0.2.0 (2026-11-14)

A big update, with new features and lots of polish throughout the app.

- New: Draft with Claude Code. `draftmux open --backend claude` continues a
  Claude Code session as the draft's own, leaving the original conversation
  as it was. Claude Code sessions draft on Claude Code by default; set
  Backend in Settings to use pi. Needs Claude Code 2.1.293 or later.
- New: `draftmux <FILE>` opens a file for editing, with an agent session of
  its own in the file's directory.
- New: `draftmux open --draft-file <path>` hands off a message your agent has
  already written.
- New: Save a draft opened from a file back to it with `⌘S` or
  `draftmux save`. It asks before replacing a file changed outside the draft.
- New: Preview a draft as a GitHub repository file, where a single line break
  shows as a space. Pick Repository File from the GitHub menu or pass
  `--target github-file`.
- New: Edit several passages with one chat message. Hold `⌘` (`Ctrl` on
  Linux) while dragging to add a selection.
- New: Draftmux on Linux updates itself like the macOS app.
- New: The update button shows what changed in every version since yours.
- New: Zoom the editor, preview and chat with `⌘+`, `⌘−` and `⌘0`.
- New: Switch a draft without an agent session between Claude Code and pi
  from the agent name above the chat.
- New: The sidebar has an Archived section, and `draftmux restore <id>`
  unarchives a draft.
- New: `draftmux focus <id>` shows a draft in the app, launching it if needed.
- New: Format has Wrap at 80 Columns and Unwrap Paragraphs.
- New: The GitHub preview highlights code blocks in GitHub's syntax colors.
- Changed: Format uses Draftmux's built-in Markdown formatter and no longer
  needs oxfmt.
- Changed: The agent writes GitHub drafts with each paragraph on one line.
- Changed: Options applied from the selection popover appear in Changes and
  can be reverted.
- Changed: The app logs to `draftmux.log` in the data directory, including
  crashes, however it was started.
- UX: Text in the review's columns is selectable.
- UX: Undo and redo reach the draft from anywhere in the window.
- UX: The editor collapses when the window is too narrow for it beside the
  preview.
- Fix: A handed-off draft whose first draft never started asks for it again
  the next time it opens.
- Fix: A `settings.json` that doesn't parse is no longer overwritten when you
  change a setting.
- Fix: When loading a draft's earlier conversation fails, the chat says so
  and offers Retry.
- Fix: While asking which version of a changed file to keep, the app stops
  saving and hides stale save errors.
- Fix: Changing a draft's details from the app and from `draftmux restore` at
  the same time no longer loses one of the changes.
- Fix: The install script checks it can replace both the app and the command
  before changing either.

Thanks [@erikjuhani](https://github.com/erikjuhani) for great feedback!

## v0.1.0 (2026-11-12)

Initial release.
