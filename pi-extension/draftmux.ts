/**
 * Draftmux extension, shipped inside Draftmux.app and loaded with `-e`.
 *
 * Configured per process through DRAFTMUX_* environment variables, and per
 * run through the draft's meta.json; see README.md for the contract with the
 * app.
 */

import { readFileSync, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export const CONTEXT_MESSAGE_TYPE = "draftmux-context";
export const DRAFT_MESSAGE_TYPE = "draftmux-draft";
export const TRUST_STATUS_KEY = "draftmux.trust";

type Target = "github" | "slack" | "rich" | "plain";

type Config =
	| { mode: "selection" }
	| { mode: "draft"; draftPath: string; instructions: string };

/** The same text as `syntax` in crates/draftmux/src/selection.rs. */
const TARGET_SYNTAX: Record<Target, string> = {
	github: "GitHub-flavored Markdown",
	slack:
		"Markdown for a Slack message: every line break shows as written; **bold**, *italic*, ~~strike~~, `code`, [text](url) links; :shortcode: emoji and <@U…> mentions work; headings paste as a bold line and tables as a code block; keep list items to one line",
	rich: "Markdown, pasted as rich text into an email or document (headings, bold, italic, links, lists, quotes, code and tables carry over; no raw HTML, alerts or @mentions; no subject line)",
	plain:
		"plain text, such as an email body (no Markdown syntax such as **bold**, # headings, backticks, [text](url) links or tables; write URLs in full; no subject line)",
};

export function readConfig(env: NodeJS.ProcessEnv): Config {
	const mode = env.DRAFTMUX_MODE || "draft";
	if (mode === "selection") return { mode };
	if (mode !== "draft") {
		throw new Error(`DRAFTMUX_MODE must be "draft" or "selection", got "${mode}"`);
	}
	const draftPath = env.DRAFTMUX_DRAFT_PATH;
	if (!draftPath || !isAbsolute(draftPath)) {
		throw new Error("DRAFTMUX_DRAFT_PATH must be set to the absolute path of draft.md");
	}
	return { mode, draftPath, instructions: env.DRAFTMUX_INSTRUCTIONS?.trim() ?? "" };
}

/** The target in the `meta.json` beside the draft file; undefined if it can't be read. */
function readTarget(draftPath: string): Target | undefined {
	try {
		let target = JSON.parse(readFileSync(join(dirname(draftPath), "meta.json"), "utf8"))?.target;
		// The app reads "email" as the plain target too.
		if (target === "email") target = "plain";
		if (typeof target === "string" && Object.hasOwn(TARGET_SYNTAX, target)) return target as Target;
	} catch {}
	return undefined;
}

/** Resolves a tool's `path` argument the way pi's edit/write tools do (`resolveToCwd`). */
function resolveToolPath(path: string, cwd: string): string {
	let p = path.replace(/[  -   　]/g, " ");
	if (p.startsWith("@")) p = p.slice(1);
	if (p === "~") p = homedir();
	else if (p.startsWith("~/")) p = join(homedir(), p.slice(2));
	else if (p.startsWith("file://")) p = fileURLToPath(p);
	return resolve(cwd, p);
}

/** Real path with symlinks and APFS case resolved; for a missing file, its parent's. */
function canonicalPath(path: string): string {
	try {
		return realpathSync.native(path);
	} catch {}
	try {
		return join(realpathSync.native(dirname(path)), basename(path));
	} catch {}
	return path;
}

function contextMessage(config: Extract<Config, { mode: "draft" }>, target: Target | undefined): string {
	const lines = [
		"[Draftmux] You are helping the user polish a message in Draftmux.",
		"",
		`Draft file: ${config.draftPath}`,
		...(target ? [`Target platform: ${TARGET_SYNTAX[target]}`] : []),
		"",
		"- Make every change to the draft by editing that file with the edit or write tool; the user sees it in the Draftmux editor. Don't paste drafts or rewritten versions in chat; use chat for discussion.",
		"- Draftmux sends you the draft's current content when it differs from what you last read, edited or wrote, for example after the user edits it. Otherwise the file is exactly as you left it, so edit it without reading it first.",
		"- Put fields in YAML frontmatter at the very top of the draft, between --- lines: title: for PRs and issues; subject:, to: and cc: for emails (only known recipients). Any other key is allowed. Use string values, quoting YAML punctuation as needed; use comma-separated strings rather than lists. Keep these fields separate from the message body. Draftmux displays them separately and copies only the body.",
		"- Change only what the user asks for; keep the rest of the draft, including its formatting, as it is.",
		"- edit and write only work on the draft file. read, bash and other tools work as usual for checking facts.",
	];
	if (config.instructions) {
		lines.push("", "Drafting instructions from the user:", config.instructions);
	}
	return lines.join("\n");
}

/** Drafts larger than this are left for the agent to read, as pi's read tool truncates at 50 KB. */
const MAX_SENT_DRAFT_BYTES = 50 * 1024;

function draftMessage(draft: string): string {
	if (Buffer.byteLength(draft) > MAX_SENT_DRAFT_BYTES) {
		return "[Draftmux] The draft file changed since you last read, edited or wrote it. Read it before editing.";
	}
	if (draft === "") return "[Draftmux] The draft file is empty.";
	return `[Draftmux] The draft file holds exactly this text, between the draft tags:\n\n<draft>\n${draft.endsWith("\n") ? draft : `${draft}\n`}</draft>`;
}

function messageText(content: unknown): string {
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return "";
	return content.map((part) => (part?.type === "text" ? part.text : "")).join("");
}

export default function draftmux(pi: ExtensionAPI) {
	const config = readConfig(process.env);

	pi.on("session_start", (_event, ctx) => {
		if (ctx.mode !== "rpc") return;
		ctx.ui.setStatus(TRUST_STATUS_KEY, ctx.isProjectTrusted() ? "trusted" : "untrusted");
	});

	if (config.mode === "selection") {
		pi.on("tool_call", () => ({
			block: true,
			reason: "Draftmux: tools are disabled for this request. Reply with text only.",
		}));
		return;
	}

	const isDraft = (resolved: string) => canonicalPath(resolved) === canonicalPath(config.draftPath);

	pi.on("tool_call", (event, ctx) => {
		if (event.toolName !== "edit" && event.toolName !== "write") return undefined;
		const requested = event.input.path;
		if (typeof requested !== "string") {
			return { block: true, reason: `Draftmux: ${event.toolName} needs a path to ${config.draftPath}.` };
		}
		const resolved = resolveToolPath(requested, ctx.cwd);
		if (isDraft(resolved)) return undefined;
		const shown = resolved === requested ? `"${requested}"` : `"${requested}" (${resolved})`;
		return {
			block: true,
			reason: `Draftmux: ${event.toolName} is limited to the draft file ${config.draftPath}; ${shown} is outside it. Use read or bash to inspect other files.`,
		};
	});

	// The app changes the target without restarting pi, so it is read before
	// every run; a failed read keeps the last one.
	let target: Target | undefined;
	pi.on("before_agent_start", (_event, ctx) => {
		target = readTarget(config.draftPath) ?? target;
		const content = contextMessage(config, target);
		const previous = ctx.sessionManager
			.buildSessionProjection()
			.messages.findLast((m) => m.role === "custom" && m.customType === CONTEXT_MESSAGE_TYPE);
		if (previous && messageText(previous.content) === content) return undefined;
		return { message: { customType: CONTEXT_MESSAGE_TYPE, content, display: false } };
	});

	// The draft text the agent last saw in full: through its own read, edit
	// or write, or a draft message. Each run starts with a draft message when
	// the file differs from it, so the agent never has to read the file to
	// catch up with the user's edits. Unknown at first and after anything that
	// may take the earlier text out of the agent's context.
	let seen: string | undefined;
	const forget = () => {
		seen = undefined;
	};
	pi.on("session_start", forget);
	pi.on("session_tree", forget);
	pi.on("session_compact", forget);

	pi.on("tool_result", (event, ctx) => {
		if (event.toolName !== "read" && event.toolName !== "edit" && event.toolName !== "write") return;
		const path = event.input.path;
		if (typeof path !== "string" || !isDraft(resolveToolPath(path, ctx.cwd))) return;
		const partial =
			event.toolName === "read" &&
			(event.input.offset !== undefined ||
				event.input.limit !== undefined ||
				(event.details as { truncation?: { truncated?: boolean } } | undefined)?.truncation?.truncated);
		if (partial) return;
		if (event.isError) {
			// A failed edit means the agent's idea of the file is off.
			if (event.toolName !== "read") forget();
			return;
		}
		seen = readDraft(config.draftPath);
	});

	pi.on("before_agent_start", () => {
		const draft = readDraft(config.draftPath);
		if (draft === undefined || draft === seen) return undefined;
		seen = Buffer.byteLength(draft) > MAX_SENT_DRAFT_BYTES ? undefined : draft;
		return { message: { customType: DRAFT_MESSAGE_TYPE, content: draftMessage(draft), display: false } };
	});
}

function readDraft(path: string): string | undefined {
	try {
		return readFileSync(path, "utf8");
	} catch {
		return undefined;
	}
}
