import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, relative } from "node:path";
import { afterEach, beforeEach, describe, test } from "node:test";
import { pathToFileURL } from "node:url";
import draftmux, { CONTEXT_MESSAGE_TYPE, readConfig, TRUST_STATUS_KEY } from "./draftmux.ts";

type Handler = (event: any, ctx: any) => any;

function load(env: Record<string, string>) {
	const saved = { ...process.env };
	for (const key of Object.keys(process.env)) if (key.startsWith("DRAFTMUX_")) delete process.env[key];
	Object.assign(process.env, env);
	const handlers = new Map<string, Handler>();
	try {
		draftmux({ on: (name: string, handler: Handler) => handlers.set(name, handler) } as any);
	} finally {
		process.env = saved;
	}
	return handlers;
}

let dir: string;
let draftPath: string;
let handlers: Map<string, Handler>;
const ctx = (extra: object = {}) => ({ cwd: join(dir, "repo"), ...extra });
const call = (toolName: string, input: object) =>
	handlers.get("tool_call")!({ type: "tool_call", toolName, toolCallId: "1", input }, ctx());

beforeEach(() => {
	dir = realpathSync(mkdtempSync(join(tmpdir(), "draftmux ext ")));
	mkdirSync(join(dir, "Draft Dir"));
	mkdirSync(join(dir, "repo"));
	draftPath = join(dir, "Draft Dir", "draft.md");
	writeFileSync(draftPath, "hello\n");
	handlers = load({ DRAFTMUX_DRAFT_PATH: draftPath, DRAFTMUX_TARGET: "github", DRAFTMUX_INSTRUCTIONS: "Be brief." });
});

afterEach(() => rmSync(dir, { recursive: true, force: true }));

describe("draft mode tool guard", () => {
	test("allows edit and write to the draft in any path form", () => {
		const forms = [
			draftPath,
			relative(join(dir, "repo"), draftPath),
			`@${draftPath}`,
			pathToFileURL(draftPath).href,
			draftPath.replace("Draft Dir", "draft dir"),
			draftPath.replace(" ", " "),
			join(dir, "repo", "..", "Draft Dir", "draft.md"),
		];
		if (draftPath.startsWith(`${homedir()}/`)) forms.push(`~/${relative(homedir(), draftPath)}`);
		for (const path of forms) {
			assert.equal(call("edit", { path, edits: [] }), undefined, path);
			assert.equal(call("write", { path, content: "x" }), undefined, path);
		}
	});

	test("allows the draft through a symlinked directory, also once deleted", () => {
		symlinkSync(join(dir, "Draft Dir"), join(dir, "alias"));
		const aliased = join(dir, "alias", "draft.md");
		assert.equal(call("write", { path: aliased, content: "x" }), undefined);
		rmSync(draftPath);
		assert.equal(call("write", { path: aliased, content: "x" }), undefined);
		assert.equal(call("write", { path: draftPath, content: "x" }), undefined);
	});

	test("blocks other files with a reason naming the draft", () => {
		for (const path of ["README.md", join(dir, "Draft Dir", "draft.md.bak"), join(dir, "Draft Dir"), "/etc/hosts"]) {
			const result = call("write", { path, content: "x" });
			assert.equal(result?.block, true, path);
			assert.match(result.reason, /limited to the draft file/);
			assert.ok(result.reason.includes(draftPath));
			assert.ok(result.reason.includes(path));
		}
		assert.equal(call("edit", { edits: [] })?.block, true);
	});

	test("leaves other tools alone", () => {
		assert.equal(call("read", { path: "/etc/hosts" }), undefined);
		assert.equal(call("bash", { command: "echo hi > /tmp/x" }), undefined);
	});
});

describe("selection mode", () => {
	test("blocks every tool and injects no context", () => {
		handlers = load({ DRAFTMUX_MODE: "selection" });
		for (const toolName of ["read", "bash", "edit", "write", "some_custom_tool"]) {
			const result = call(toolName, { path: draftPath });
			assert.equal(result?.block, true);
			assert.match(result.reason, /tools are disabled/);
		}
		assert.equal(handlers.has("before_agent_start"), false);
	});
});

describe("context message", () => {
	const start = (messages: object[]) =>
		handlers.get("before_agent_start")!(
			{ type: "before_agent_start", messageType: "user", prompt: "hi" },
			ctx({ sessionManager: { buildSessionProjection: () => ({ messages }) } }),
		);

	test("carries draft path, target, instructions and editing rules", () => {
		const result = start([]);
		assert.equal(result.systemPrompt, undefined);
		assert.equal(result.message.customType, CONTEXT_MESSAGE_TYPE);
		assert.equal(result.message.display, false);
		const parts = [draftPath, "GitHub", "Be brief.", "Don't paste drafts", "Change only what the user asks for"];
		for (const part of parts) {
			assert.ok(result.message.content.includes(part), part);
		}
	});

	test("is sent once until its content changes", () => {
		const sent = { role: "custom", customType: CONTEXT_MESSAGE_TYPE, content: start([]).message.content };
		assert.equal(start([sent, { role: "user", content: "x" }]), undefined);
		assert.equal(start([{ ...sent, content: [{ type: "text", text: sent.content }] }]), undefined);
		assert.ok(start([{ ...sent, content: "old instructions" }]).message);
		handlers = load({ DRAFTMUX_DRAFT_PATH: draftPath, DRAFTMUX_TARGET: "slack" });
		const slack = start([sent]).message.content;
		assert.match(slack, /Slack mrkdwn/);
		assert.match(slack, /headings and tables are fine/);
		handlers = load({ DRAFTMUX_DRAFT_PATH: draftPath, DRAFTMUX_TARGET: "email" });
		assert.match(start([sent]).message.content, /plain-text email \(no Markdown/);
	});
});

describe("trust status", () => {
	for (const trusted of [true, false]) {
		test(`reports ${trusted ? "trusted" : "untrusted"} in rpc mode`, () => {
			const statuses: [string, string][] = [];
			const ui = { setStatus: (key: string, text: string) => statuses.push([key, text]) };
			handlers.get("session_start")!({}, ctx({ mode: "rpc", ui, isProjectTrusted: () => trusted }));
			handlers.get("session_start")!({}, ctx({ mode: "print", ui, isProjectTrusted: () => trusted }));
			assert.deepEqual(statuses, [[TRUST_STATUS_KEY, trusted ? "trusted" : "untrusted"]]);
		});
	}
});

describe("config", () => {
	test("rejects missing or invalid values", () => {
		assert.throws(() => readConfig({ DRAFTMUX_TARGET: "github" }), /DRAFTMUX_DRAFT_PATH/);
		assert.throws(() => readConfig({ DRAFTMUX_DRAFT_PATH: "draft.md", DRAFTMUX_TARGET: "github" }), /absolute/);
		assert.throws(() => readConfig({ DRAFTMUX_DRAFT_PATH: "/d.md", DRAFTMUX_TARGET: "teams" }), /DRAFTMUX_TARGET/);
		assert.throws(() => readConfig({ DRAFTMUX_MODE: "chat" }), /DRAFTMUX_MODE/);
		assert.deepEqual(readConfig({ DRAFTMUX_MODE: "selection" }), { mode: "selection" });
	});
});
