# Draftmux pi extension

`draftmux.ts` is the pi extension Draftmux passes with `-e` to every pi process
it starts. Nothing is installed in the user's pi. It:

- limits `edit` and `write` to the draft file, leaving `read`, `bash` and other
  tools available,
- tells the agent the draft path, target platform and drafting instructions
  through a message, never through the system prompt,
- blocks every tool call in selection calls,
- reports whether pi trusts the project.

It has no runtime dependencies; pi loads the TypeScript file directly. Tests:
`npm test` (Node 22.18 or later).

## Configuration

The app sets these environment variables on the pi process. They are read once
when pi loads the extension, so changing the target or instructions means
respawning the process. bash commands the agent runs inherit them.

| Variable                | Value                                                    |
| ----------------------- | -------------------------------------------------------- |
| `DRAFTMUX_MODE`         | `draft` (default when unset) or `selection`              |
| `DRAFTMUX_DRAFT_PATH`   | Absolute path of the draft's `draft.md`; draft mode only |
| `DRAFTMUX_TARGET`       | `github`, `slack` or `email`; draft mode only            |
| `DRAFTMUX_INSTRUCTIONS` | The user's drafting instructions, any text; optional     |

A missing or invalid value makes pi exit with status 1 and an error on stderr
naming the variable, so pi never runs unguarded.

## Invocations

All three run with cwd = the draft's working directory and
`PI_CODING_AGENT_DIR` = the draft's profile. `<ext>` is the bundled
`draftmux.ts`.

```sh
# Handoff: fork the source session (draft mode)
pi --mode rpc --fork <source.jsonl> --session-dir <draft>/session -e <ext>

# Later starts of the draft (draft mode)
pi --mode rpc --session <draft session file> -e <ext>

# Selection call (DRAFTMUX_MODE=selection), stdin closed
pi -p "<prompt>" --fork <draft session file> --session-dir <temp dir> -e <ext>
```

Never pass `--append-system-prompt`, `--no-tools` or `--tools`: the system
prompt and tool list must stay identical to the source session's for the
prompt cache.

## Behaviour the app relies on

### Draft writes

In draft mode an `edit` or `write` either targets the draft or is blocked, so
every `tool_execution_end` with `toolName` `edit` or `write` and
`isError: false` changed `draft.md`. Reload on that event. Don't compare `tool_execution_start`'s
`args.path` to the draft path: it is the path as the model wrote it (relative,
`~/…`, a symlink or different letter case are all allowed).

A blocked call ends with `isError: true` and this text, which the model also
sees:

```text
Draftmux: write is limited to the draft file /…/draft.md; "notes.txt" (/repo/notes.txt) is outside it. Use read or bash to inspect other files.
```

The guard keeps the agent on the draft; it is not a sandbox. `bash` and tools
from the user's own pi extensions can still change any file.

### Context message

Before a run, the extension adds a custom message with
`customType: "draftmux-context"` and `display: false` after the user's prompt.
It holds the draft path, target platform, drafting instructions and the
instruction to edit the file rather than paste drafts in chat. pi saves it in
the session, and the chat pane should hide it. It is added again only when its
text changes, for example after a respawn with a different target, so a long
chat holds one copy. Selection calls add none; their prompt carries the draft
text and selection.

### Selection calls

Every tool call is blocked with `Draftmux: tools are disabled for this request.
Reply with text only.` The model gets that as the tool result and answers in
text.

### Project trust

In RPC mode the extension prints this line on stdout as pi starts, before the
app's first command is answered, and again whenever pi starts a new session in
the same process:

```json
{"type":"extension_ui_request","id":"…","method":"setStatus","statusKey":"draftmux.trust","statusText":"trusted"}
```

`statusText` is `trusted` or `untrusted` (`ctx.isProjectTrusted()`). It needs no
reply. When the project is untrusted, pi ignores its `.pi/settings.json` and
other project-local resources.
