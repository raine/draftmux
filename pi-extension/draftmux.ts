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
export const TRUST_STATUS_KEY = "draftmux.trust";

type Target = "github" | "slack" | "email";

type Config =
	| { mode: "selection" }
	| { mode: "draft"; draftPath: string; instructions: string };

/** The same text as `syntax` in crates/draftmux/src/selection.rs. */
const TARGET_SYNTAX: Record<Target, string> = {
	github: "GitHub-flavored Markdown",
	slack:
		"Slack mrkdwn (*bold*, _italic_, ~strike~, `code`, <url|text>; Markdown headings and tables are fine and paste into Slack as a bold line and a code block)",
	email:
		"plain-text email (no Markdown syntax such as **bold**, # headings, backticks, [text](url) links or tables; write URLs in full; no subject line)",
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
		const target = JSON.parse(readFileSync(join(dirname(draftPath), "meta.json"), "utf8"))?.target;
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
		"- Read the file before editing: the user may have changed it since your last edit.",
		"- Change only what the user asks for; keep the rest of the draft, including its formatting, as it is.",
		"- edit and write only work on the draft file. read, bash and other tools work as usual for checking facts.",
	];
	if (config.instructions) {
		lines.push("", "Drafting instructions from the user:", config.instructions);
	}
	return lines.join("\n");
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

	pi.on("tool_call", (event, ctx) => {
		if (event.toolName !== "edit" && event.toolName !== "write") return undefined;
		const requested = event.input.path;
		if (typeof requested !== "string") {
			return { block: true, reason: `Draftmux: ${event.toolName} needs a path to ${config.draftPath}.` };
		}
		const resolved = resolveToolPath(requested, ctx.cwd);
		if (canonicalPath(resolved) === canonicalPath(config.draftPath)) return undefined;
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
}
