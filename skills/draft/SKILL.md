---
name: draft
description: Hand a GitHub, Slack or email message off to the Draftmux app for polishing, and read the polished draft back. Use when the user asks to draft, write or polish a message (PR description, issue or PR comment, commit message, Slack message, email) in Draftmux, or runs /draft. Also use when the user says a Draftmux draft is done, ready, or should be used or posted, including after the conversation was compacted.
---

# Draftmux draft

Draftmux opens the message in its own window, with this conversation as
context, so the user can polish it there. Later, when the user says the draft
is done, read it back and use it.

## Locate this session

Every command below starts with these lines. Shell variables don't survive
between commands, so repeat them each time; never reuse a path from memory.

```sh
f=
if [ -n "${DRAFTMUX_DRAFT_PATH:-}" ]; then
  echo "draft: this session is already a Draftmux draft; edit $DRAFTMUX_DRAFT_PATH directly" >&2
  exit 1
elif [ -n "${PI_SESSION_ID:-}" ]; then
  f=${PI_SESSION_FILE:-}
elif [ -n "${CLAUDE_CODE_SESSION_ID:-}" ]; then
  f=$(find "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects" -mindepth 2 -maxdepth 2 \
    -name "$CLAUDE_CODE_SESSION_ID.jsonl" -print -quit)
elif [ "${PI_CODING_AGENT:-}" = true ]; then
  echo "draft: pi didn't set PI_SESSION_ID for bash; a pi extension that replaces the bash tool may not pass it through" >&2
fi
[ -f "$f" ] || { echo "draft: can't find this session's file" >&2; exit 1; }
```

If it fails, report the error to the user. Don't guess the session file by
other means.

## Hand off

Run the locator, then:

```sh
set -- "$f"
[ -n "${PI_CODING_AGENT_DIR:-}" ] && set -- "$@" --pi-agent-dir "$PI_CODING_AGENT_DIR"
draftmux open "$@" --target github --note 'PR description for the retry change'
```

- `--pi-agent-dir`: passed only when `PI_CODING_AGENT_DIR` is set, as above.
  Otherwise Draftmux picks the profile from the session file's location or its
  own settings.
- `--target`: `github` for GitHub markdown, `slack` for Slack messages,
  `email` for plain-text emails. Leave it out when the user hasn't said where
  the message goes; Draftmux then uses its default for the directory.
- `--note`: optional. One line saying what message to write, if the
  conversation doesn't make it obvious. Single-quote it; write `'` as `'\''`.

It prints the draft ID. Tell the user the ID and that the draft is open in
Draftmux, then stop and wait. Don't write the message yourself.

If `open` exits 1 after printing an ID, the draft exists but the app couldn't
focus it: report the error and the ID, and don't run `open` again (that
creates a second draft). If it exits 1 without an ID, nothing was created;
report the error.

## Read it back

When the user says the draft is done, run the locator again, then read the
draft from Draftmux. Use the text exactly as it is: the user has already
polished it, so don't rewrite, reformat, shorten or fix it.

For a command that takes a file, pass the draft's path. This keeps backticks,
`$` and quotes in the body intact. The path contains a space, so always quote
it, and check `show` succeeded before using it:

```sh
p=$(draftmux show --source "$f" --path) || exit 1
gh pr create --title 'Retry failed uploads' --body-file "$p"
```

Other file-taking commands work the same way: `gh pr comment --body-file "$p"`,
`gh issue comment --body-file "$p"`, `git commit -F "$p"`.

Only when the destination has no file option (for example a Slack tool call),
print the text and pass it on character for character:

```sh
draftmux show --source "$f"
```

If several drafts came from this session, `show` uses the most recently active
one and lists the others on stderr; mention them to the user. If it finds no
draft (for example the session was resumed under a new file) and you still
have the ID from `open`, use `draftmux show <id>` or
`draftmux show <id> --path` instead. Otherwise ask the user. Never run `open`
to recover a draft.
