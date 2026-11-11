import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, relative } from "node:path";
import { afterEach, beforeEach, describe, test } from "node:test";
import { pathToFileURL } from "node:url";
import draftmux, { CONTEXT_MESSAGE_TYPE, DRAFT_MESSAGE_TYPE, readConfig, TRUST_STATUS_KEY } from "./draftmux.ts";

type Handler = (event: any, ctx: any) => any;

function load(env: Record<string, string>) {
	const saved = { ...process.env };
	for (const key of Object.keys(process.env)) if (key.startsWith("DRAFTMUX_")) delete process.env[key];
	Object.assign(process.env, env);
	const handlers = new Map<string, Handler[]>();
	const on = (name: string, handler: Handler) => handlers.set(name, [...(handlers.get(name) ?? []), handler]);
	try {
		draftmux({ on } as any);
	} finally {
		process.env = saved;
	}
	return handlers;
}

let dir: string;
let draftPath: string;
const writeMeta = (content: string) => writeFileSync(join(dir, "Draft Dir", "meta.json"), content);
const setTarget = (target: string) => writeMeta(JSON.stringify({ target, status: "active" }));
let handlers: Map<string, Handler[]>;
const ctx = (extra: object = {}) => ({ cwd: join(dir, "repo"), ...extra });
/** Runs every handler for the event, returning the first result as pi's tool_call does. */
const emit = (name: string, event: object, context = ctx()) =>
	(handlers.get(name) ?? []).map((handler) => handler({ type: name, ...event }, context)).find((r) => r !== undefined);
const call = (toolName: string, input: object) => emit("tool_call", { toolName, toolCallId: "1", input });

beforeEach(() => {
	dir = realpathSync(mkdtempSync(join(tmpdir(), "draftmux ext ")));
	mkdirSync(join(dir, "Draft Dir"));
	mkdirSync(join(dir, "repo"));
	draftPath = join(dir, "Draft Dir", "draft.md");
	writeFileSync(draftPath, "hello\n");
	setTarget("github");
	handlers = load({ DRAFTMUX_DRAFT_PATH: draftPath, DRAFTMUX_INSTRUCTIONS: "Be brief." });
});

afterEach(() => rmSync(dir, { recursive: true, force: true }));

describe("draft mode tool guard", () => {
	test("allows edit and write to the draft in any path form", () => {
		const forms = [
			draftPath,
			relative(join(dir, "repo"), draftPath),
			`@${draftPath}`,
			pathToFileURL(draftPath).href,
			draftPath.replace(" ", " "),
			join(dir, "repo", "..", "Draft Dir", "draft.md"),
		];
		// Another case names the same file only on a case-insensitive filesystem.
		const otherCase = draftPath.replace("Draft Dir", "draft dir");
		if (existsSync(otherCase)) forms.push(otherCase);
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

/** Every before_agent_start result for a run, given the session's messages so far. */
const startRun = (messages: object[] = []): any[] =>
	(handlers.get("before_agent_start") ?? [])
		.map((handler) =>
			handler(
				{ type: "before_agent_start", messageType: "user", prompt: "hi" },
				ctx({ sessionManager: { buildSessionProjection: () => ({ messages }) } }),
			),
		)
		.filter((result) => result !== undefined);

describe("context message", () => {
	const start = (messages: object[]) =>
		startRun(messages).find((result) => result.message?.customType === CONTEXT_MESSAGE_TYPE);

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

	const sent = (content: string) => ({ role: "custom", customType: CONTEXT_MESSAGE_TYPE, content });

	test("is sent once until its content changes", () => {
		const github = sent(start([]).message.content);
		assert.equal(start([github, { role: "user", content: "x" }]), undefined);
		assert.equal(start([{ ...github, content: [{ type: "text", text: github.content }] }]), undefined);
		assert.ok(start([{ ...github, content: "old instructions" }]).message);
	});

	test("follows the target in meta.json from run to run", () => {
		const github = sent(start([]).message.content);
		setTarget("slack");
		const slack = start([github]).message.content;
		assert.match(slack, /Markdown for a Slack message/);
		assert.match(slack, /headings paste as a bold line/);
		assert.equal(start([sent(slack)]), undefined);
		setTarget("rich");
		const rich = start([sent(slack)]).message.content;
		assert.match(rich, /Markdown, pasted as rich text/);
		setTarget("plain");
		const plain = start([sent(rich)]).message.content;
		assert.match(plain, /plain text, such as an email body \(no Markdown/);
		setTarget("email");
		assert.equal(start([sent(plain)]), undefined);
		setTarget("github");
		assert.equal(start([github]), undefined);
	});

	test("keeps the last target when meta.json can't be read", () => {
		setTarget("slack");
		const slack = sent(start([]).message.content);
		for (const broken of ["{ not json", JSON.stringify({ target: "teams" }), "null"]) {
			writeMeta(broken);
			assert.equal(start([slack]), undefined, broken);
		}
		rmSync(join(dir, "Draft Dir", "meta.json"));
		assert.equal(start([slack]), undefined);
	});

	test("leaves out the target until meta.json has one", () => {
		rmSync(join(dir, "Draft Dir", "meta.json"));
		handlers = load({ DRAFTMUX_DRAFT_PATH: draftPath });
		const content = start([]).message.content;
		assert.ok(content.includes(draftPath));
		assert.match(content, /YAML frontmatter/);
		assert.match(content, /title: for PRs and issues/);
		assert.match(content, /subject:, to: and cc: for emails/);
		assert.doesNotMatch(content, /Target platform/);
		setTarget("plain");
		assert.match(start([sent(content)]).message.content, /Target platform: plain text/);
	});
});

describe("draft message", () => {
	/** The draft message's text for a run, or undefined when the run has none. */
	const draftSent = (): string | undefined =>
		startRun().find((result) => result.message.customType === DRAFT_MESSAGE_TYPE)?.message.content;
	const toolResult = (toolName: string, input: object, extra: object = {}) =>
		emit("tool_result", { toolName, toolCallId: "1", input, content: [], isError: false, ...extra });

	test("sends the draft once, then again only after it changes", () => {
		const first = startRun().find((result) => result.message.customType === DRAFT_MESSAGE_TYPE);
		assert.equal(first.message.display, false);
		assert.match(first.message.content, /<draft>\nhello\n<\/draft>/);
		assert.equal(draftSent(), undefined);
		writeFileSync(draftPath, "hello there\n");
		assert.match(draftSent()!, /<draft>\nhello there\n<\/draft>/);
		assert.equal(draftSent(), undefined);
		writeFileSync(draftPath, "no newline");
		assert.match(draftSent()!, /<draft>\nno newline\n<\/draft>/);
		writeFileSync(draftPath, "");
		assert.match(draftSent()!, /draft file is empty/);
	});

	test("counts the agent's own reads, edits and writes of the draft as seen", () => {
		for (const toolName of ["read", "edit", "write"]) {
			emit("session_start", {}, ctx({ mode: "print" }));
			writeFileSync(draftPath, `after ${toolName}\n`);
			toolResult(toolName, { path: relative(join(dir, "repo"), draftPath) });
			assert.equal(draftSent(), undefined, toolName);
		}
	});

	test("ignores partial reads and other files", () => {
		const unseen = [
			["read", { path: draftPath, offset: 2 }],
			["read", { path: draftPath, limit: 1 }],
			["read", { path: draftPath }, { details: { truncation: { truncated: true } } }],
			["read", { path: draftPath }, { isError: true }],
			["write", { path: join(dir, "repo", "notes.md") }],
			["bash", { command: `cat ${draftPath}` }],
		] as const;
		for (const [toolName, input, extra] of unseen) {
			emit("session_start", {}, ctx({ mode: "print" }));
			toolResult(toolName, input, extra);
			assert.ok(draftSent(), `${toolName} ${JSON.stringify(input)}`);
		}
	});

	test("sends the draft again after a failed edit", () => {
		draftSent();
		toolResult("edit", { path: draftPath }, { isError: true });
		assert.ok(draftSent());
	});

	for (const event of ["session_start", "session_tree", "session_compact"]) {
		test(`sends the draft again after ${event}`, () => {
			draftSent();
			emit(event, {}, ctx({ mode: "print" }));
			assert.match(draftSent()!, /<draft>\nhello\n<\/draft>/);
		});
	}

	test("asks for a read instead of sending a large draft", () => {
		writeFileSync(draftPath, "x".repeat(60 * 1024));
		for (let i = 0; i < 2; i++) {
			const text = draftSent()!;
			assert.match(text, /Read it before editing/);
			assert.doesNotMatch(text, /<draft>/);
		}
		toolResult("read", { path: draftPath });
		assert.equal(draftSent(), undefined);
	});

	test("sends nothing when the draft can't be read", () => {
		rmSync(draftPath);
		assert.equal(draftSent(), undefined);
	});
});

describe("trust status", () => {
	for (const trusted of [true, false]) {
		test(`reports ${trusted ? "trusted" : "untrusted"} in rpc mode`, () => {
			const statuses: [string, string][] = [];
			const ui = { setStatus: (key: string, text: string) => statuses.push([key, text]) };
			emit("session_start", {}, ctx({ mode: "rpc", ui, isProjectTrusted: () => trusted }));
			emit("session_start", {}, ctx({ mode: "print", ui, isProjectTrusted: () => trusted }));
			assert.deepEqual(statuses, [[TRUST_STATUS_KEY, trusted ? "trusted" : "untrusted"]]);
		});
	}
});

describe("config", () => {
	test("rejects missing or invalid values", () => {
		assert.throws(() => readConfig({}), /DRAFTMUX_DRAFT_PATH/);
		assert.throws(() => readConfig({ DRAFTMUX_DRAFT_PATH: "draft.md" }), /absolute/);
		assert.throws(() => readConfig({ DRAFTMUX_MODE: "chat" }), /DRAFTMUX_MODE/);
		assert.deepEqual(readConfig({ DRAFTMUX_MODE: "selection" }), { mode: "selection" });
	});
});
