// The demo scene, shared by its two cuts. index.html is the website cut: it
// opens on the session's scrollback fading in, and the landing page plays
// it. video.html is the video cut, a standalone demo for the README and
// video sites: a title card first, then the same story, with the wordmark in
// the corner over the opening shots. A cut sets window.CUT before loading
// this file.
//
// Everything on screen is a pure function of the story time (seconds). The
// video cut's title card plays before the story starts, so its scene time
// runs LEAD seconds ahead of story time; T, CAPTIONS, CAMERA and CURSOR are
// all in story time.
const VIDEO = window.CUT === 'video';
const LEAD = VIDEO ? 3.1 : 0;
// When the video cut's title card lifts off, in story time.
const TITLE = [.2, .8];
document.body.insertAdjacentHTML('afterbegin', `<div id="stage">
  <div id="world">
    <div id="term" class="window">
      <div class="bar"><div class="traffic"><i></i><i></i><i></i></div>tidepool — claude</div>
      <div class="tbody" id="tbody"></div>
      <div class="dim" id="tdim"></div>
    </div>
    <div id="dmx" class="window main-win light"></div>
    <img id="lb" src="img/load-bearing.webp" />
    <svg id="cursor" viewBox="0 0 24 34"><path d="M3 2 L3 26 L9 20.5 L13 30 L17 28.3 L13.2 19 L21 19 Z" fill="#111" stroke="#fff" stroke-width="2" stroke-linejoin="round"/></svg>
  </div>
${VIDEO ? `  <div id="title"><img class="hero" /><div class="l1">Drafting messages with your agent</div>
    <div class="l2">What if there was a better UI for polishing message drafts?</div></div>
  <div class="brand" id="brand"><img src="img/app-icon.webp" />Draftmux</div>
` : ''}  <div id="outro"><div class="uses"><h2>Many more use cases</h2><ul></ul><div class="ex light"></div></div>
    <div class="card"><img src="img/app-icon.webp" /><div class="name">Draftmux</div><div class="l2">Now on Mac, Linux and Windows</div><div class="l3">Drafts with Pi or Claude Code, built on GPUI</div></div>
    <div class="rule"></div><div class="credits"><div class="roll"></div></div></div>
  <div id="keys"></div>
  <div id="cap"><div class="pill" id="pill"></div><div class="sub" id="sub"></div></div>
</div>`);

const I = window.ICONS, MD = window.MD;
const $ = (id) => document.getElementById(id);
const clamp = (x, a = 0, b = 1) => Math.min(b, Math.max(a, x));
const ease = (x) => { x = clamp(x); return x < .5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2; };
// Camera moves: peaks at 1.875x mean speed, against the cubic's 3x.
const smoother = (x) => { x = clamp(x); return x * x * x * (x * (6 * x - 15) + 10); };
// Settles with a slight overshoot, like a spring.
const easeOutBack = (x, c = .9) => { x = clamp(x) - 1; return 1 + (c + 1) * x * x * x + c * x * x; };
const easeOut = (x) => 1 - Math.pow(1 - clamp(x), 3);
const fade = (t, t0, d = .35) => ease((t - t0) / d);
const win = (t, a, b, d = .3) => fade(t, a, d) * (1 - fade(t, b, d));
const esc = MD.esc;
const el = (html) => { const d = document.createElement('div'); d.innerHTML = html.trim(); return d.firstElementChild; };
const setHtml = (e, s) => { if (e.dataset.html !== s) { e.dataset.html = s; e.innerHTML = s; } };
const typed = (s, t, a, b) => s.slice(0, Math.round(s.length * clamp((t - a) / (b - a))));

// ---------- Timeline ----------
const T = {
  // The session's scrollback fades in, line by line; then /draftmux is typed.
  recall: [0.1, 1.9], type1: [2.5, 3.9], enter1: 4.1, bash1: 4.5, bash1Done: 5.3, opened: 5.9, note: [4.8, 8.8],
  launch: 7.25, sbIn: 7.7, sbHide: 12.3,
  read1: 9.0, write1: 9.7, d1: 10.7, reply1: [11.3, 12.7],
  click2: 17.4, type2: [17.6, 19.2], send2: 19.5, read2: 20.0, edit2: 20.6, d2: 21.8, reply2: [22.2, 23.8],
  // The Markdown: click the end of a step in the editor and type the version.
  toggle: 24.8, mdClick: 25.6, typeMd: [26.2, 27.3],
  // Continue a sentence: click the end of the description, type its start,
  // ⌘↩ waits on the agent, its rest shows as a ghost, and ⇥ takes it.
  contClick: 28.0, typeCont: [28.4, 29.1], cont: 29.45, ghost: 30.4, accept: 31.6,
  slack: 33.9, rich: 35.3, github: 36.8, targets: [32.7, 37.5],
  drag3: [39.0, 39.8], pop3: 40.0, alt3: 40.8, opts3: 41.8, hover3: 42.2, apply3: 42.7,
  // Comment from the popover: select a step, Comment, type into its field, send.
  drag4: [43.3, 43.8], pop4: 43.95, comment4: 44.85, type4: [45.2, 46.3], send4: 46.6, edit4: 47.2, d4: 48.2, reply4: [48.4, 49.6], ring4: [47.1, 50.8],
  // Review: open Changes, compare since Alternatives, and Done keeps it all.
  review: 52.3, range: 53.2, since: 54.2, done: 55.8,
  swap: 57.9, type5: [58.8, 59.5], enter5: 59.7, show5: 60.1, show5Done: 60.6, gh5: 60.9, url5: 61.7, done5: 62.7,
  // The first draft's tics light up one by one; the load-bearing joke rides
  // on the second.
  tics: 13.7, lb: [13.95, 15.15],
  outro: 63.7, uses: [64.2, 68.7], card: 68.9,
  // The end card slides aside and the credits roll up beside it.
  squeeze: 71.0, roll: 71.8,
};
// The video cut opens on the session already on screen, under the title card.
if (VIDEO) T.recall = [-LEAD - 2, -LEAD - 1];
// How the demo was made, measured from the transcripts of the sessions that
// made it: [role, credit, note]. Running time is the cut's own length.
const CREDITS = [
  ['Directed by', 'Raine'],
  ['Written by', 'Claude Opus 5.5'],
  ['Additional material', 'Claude Fable 5.2', 'and GPT, by way of Pi'],
  ['Second opinions', '15 consultations', 'via consult-llm'],
  ['Continuity', '48 notes', 'pinned to frames in scenecast'],
  ['Camera', '105 screenshots'],
  ['Grips', '2,085 tool calls'],
  ['Catering', '331M tokens', 'mostly reheated from cache'],
  ['Running time', null],
  ['Making time', 'about 11 hours', 'over 5 evenings'],
];
// Every credit takes the same height (scene.css), so the roll's length, and
// with it the scene's, follows from the number of credits: the roll runs
// until the last credit clears the top, and the frame fades to black as it
// goes.
const CREDIT_H = 160, ROLL_SPEED = 150;
T.dark = T.roll + (1080 + CREDITS.length * CREDIT_H) / ROLL_SPEED - 1.2;
const DURATION = T.dark + 2;
const SEGMENTS = [
  ['session', 'Session', 0.0, 7.4], ['handoff', 'Hand-off', 7.4, 9.0],
  ['draft', 'First draft', 9.0, 17.2], ['chat', 'Chat', 17.2, 24.0], ['markdown', 'Markdown', 24.0, 27.8],
  ['continue', 'Continuation', 27.8, 32.2], ['targets', 'Preview', 32.2, 38.0],
  ['alts', 'Alternatives', 38.0, 43.3], ['part', 'This part', 43.3, 51.2], ['review', 'Review', 51.2, 57.3], ['send', 'Send', 57.3, T.outro], ['uses', 'Use cases', T.outro, T.card], ['end', 'End', T.card, T.squeeze], ['credits', 'Credits', T.squeeze, DURATION],
].map(([id, label, start, end]) => ({ id, label, start, end }));

// ---------- Draft states ----------
const S2 = 'This can cause a lot of load on the server.';
const S2_NEW = 'In our app, one rate-limited client sent 40 retries a second.';
const STEP = 'The retry fires right away instead of waiting';
const STEP_NEW = 'The retry fires at once instead of waiting until 14:30:00 GMT';
const TESTED = 'We parse the header ourselves and pass the delay to the retry policy. Happy to open a PR that does this inside RetryClient.';
// The first draft packs in the house style of a model writing for everyone;
// the humanizer pass keeps the facts and drops the performance.
const D1 = `---
title: Retry-After handling quietly breaks on HTTP-date values
---

## 🐛 Summary

Here's the thing: \`RetryClient\`'s handling of \`Retry-After\` is **load-bearing**, and it genuinely breaks when the header is an HTTP date. This isn't just a parsing edge case — it's a structural gap in how retrykit honors server backpressure.

## Why This Matters

Honestly, this matters more than it looks. When the parse fails, retrykit quietly falls back to zero delay, so every rate-limited client retries instantly. The result? A retry storm, landing squarely on the server.

## Steps to Reproduce

1. Respond with \`429\` and \`Retry-After: Wed, 11 Nov 2026 14:30:00 GMT\`
2. Send a request through \`RetryClient\` with the default policy
3. Watch the retry land instantly — no backoff, no wait

## Workaround

We leaned into parsing the header ourselves and passing the delay to the retry policy. It's a dependable stopgap, and a testament to how small the real fix is.

Happy to unpack any of this further! 🙏
`;
const D2 = `---
title: Retry-After is ignored when it's an HTTP date
---

## Description

\`RetryClient\` only parses \`Retry-After\` as a number of seconds. When the server sends an HTTP date, the parse fails and the retry fires with no delay. ${S2}

## Steps to reproduce

- Respond with \`429\` and \`Retry-After: Wed, 11 Nov 2026 14:30:00 GMT\`
- Send a request through \`RetryClient\` with the default policy
- The retry fires right away instead of waiting

Headers in seconds (\`Retry-After: 120\`) work as expected.

## Workaround

${TESTED}
`;
// The edit typed into the Markdown: the version the session found.
const POLICY = 'with the default policy', MD_ADD = ' in retrykit 0.7.2';
const MD_AT = D2.indexOf(POLICY) + POLICY.length;
const D2v = D2.slice(0, MD_AT) + MD_ADD + D2.slice(MD_AT);
// The sentence continued at the end of the description: typed, then the
// agent's continuation, which names the parser the session found.
const CONT_AT = D2.indexOf(S2) + S2.length, CONT_TYPED = ' The parser is', CONT_GHOST = ' `parse_retry_after` in `src/policy.rs`.';
const D2e = D2v.slice(0, CONT_AT) + CONT_TYPED + CONT_GHOST + D2v.slice(CONT_AT);
const D3a = D2e.replace(S2, S2_NEW);
const D4 = D3a.replace(STEP, STEP_NEW);
function draftAt(t) {
  if (t < T.d1) return '';
  if (t < T.d2) return D1;
  if (t < T.typeMd[0]) return D2;
  if (t < T.typeCont[0]) return D2.slice(0, MD_AT) + typed(MD_ADD, t, ...T.typeMd) + D2.slice(MD_AT);
  if (t < T.accept) return D2v.slice(0, CONT_AT) + typed(CONT_TYPED, t, ...T.typeCont) + D2v.slice(CONT_AT);
  if (t < T.apply3) return D2e;
  if (t < T.d4) return D3a;
  return D4;
}
// The continuation's ghost after the cursor: none while ⌘↩ waits on the
// agent, then its words as they stream in, until ⇥ takes them all.
function ghostAt(t) {
  if (t < T.cont || t >= T.accept) return null;
  return { at: CONT_AT + CONT_TYPED.length, text: t < T.ghost ? '' : streamed(CONT_GHOST, t, [T.ghost, T.ghost + .45]), since: T.cont };
}
// How far waiting dot i has risen, 0 at rest to 1 at the top: a wave across
// the dots every 1.2 s, as the editor paints them (element.rs).
function dotLift(t, i) {
  const p = ((((t - .16 * i) % 1.2) + 1.2) % 1.2) / 1.2;
  return p < .6 ? Math.sin(p / .6 * Math.PI) : 0;
}
const lineOf = (text, offset) => text.slice(0, offset).split('\n').length;

// Editor selection: [start, end, active] in the current draft text.
function selectionAt(t, text) {
  if (t >= T.drag3[0] && t < T.apply3 + .01) {
    const a = D2e.indexOf(S2);
    return [a, a + Math.round(S2.length * ease((t - T.drag3[0]) / (T.drag3[1] - T.drag3[0]))), true];
  }
  const ps = previewSelAt(t);
  if (ps) { const a = text.indexOf(ps[0]); return [a, a + ps[1], ps[2]]; }
  return null;
}
// The preview's selection, which the app mirrors to the Markdown as the
// editor selection above: [text, characters selected, active].
function previewSelAt(t) {
  if (t >= T.drag3[0] && t < T.apply3) return [S2, Math.round(S2.length * ease((t - T.drag3[0]) / (T.drag3[1] - T.drag3[0]))), true];
  if (t >= T.apply3 && t < T.drag4[0]) return [S2_NEW, S2_NEW.length, true];
  if (t >= T.drag4[0] && t < T.d4) return [STEP, Math.round(STEP.length * ease((t - T.drag4[0]) / (T.drag4[1] - T.drag4[0]))), t < T.send4];
  return null;
}
// A Range over the first n characters of text in the preview.
function previewRange(text, n = text.length) {
  const r = findRange($('pvbody'), text);
  if (r && n < text.length) r.setEnd(r.startContainer, r.startOffset + n);
  return r;
}
// Whether the editor's selection is attached to the composer (shown as a chip).
// It stays attached from the first selection until the message is sent.
const CHIP_IN = T.drag3[1] + .25;
function chipAt(t, text) {
  const s = selectionAt(t, text);
  if (!s || s[1] - s[0] < 2 || t >= T.send4) return null;
  if (t < CHIP_IN) return null;
  const quote = text.slice(s[0], s[1]);
  return { text: quote, startLine: lineOf(text, s[0]), endLine: lineOf(text, s[1]) };
}

// The GitHub skeleton from chrome/skeleton.rs: [kind, widths] per block. Lines
// brighten in the chat shimmer's wave, 90 ms apart, from alpha .08 to .14.
const SKELETON = [['h2', [.26]], ['p', [.97, .91, .42]], ['p', [.88, .64]], ['li', [.71, .58, .76]]];
function skeletonHtml(t) {
  let ix = 0;
  const shade = () => {
    const p = ((((t - ix++ * .09) % 1.8) + 1.8) % 1.8) / 1.8;
    const r = p < .15 ? p / .15 : p < .4 ? 1 - (p - .15) / .25 : 0;
    return (.08 + .06 * r * r * (3 - 2 * r)).toFixed(3);
  };
  const tone = (a) => `background:color-mix(in srgb, var(--md-text) ${(a * 100).toFixed(1)}%, transparent)`;
  const blocks = SKELETON.map(([kind, ws]) => ws.map((w) => {
    const a = shade();
    if (kind === 'li') return `<div class="ln li"><b style="${tone(a * 1.5)}"></b><i style="width:${w * 100}%;${tone(a)}"></i></div>`;
    return `<div class="ln${kind === 'h2' ? ' h2' : ''}"><i style="width:${w * 100}%;${tone(a)}"></i></div>`;
  }).join('')).join('<div class="gap"></div>');
  return `<div class="md t-github sk">${blocks}</div>`;
}

// ---------- Chat transcript ----------
const FIRST = 'Write the first draft of the message from our conversation.';
const NOTE = 'Issue on retrykit: Retry-After as an HTTP date is ignored';
const R1 = 'Wrote the issue: what breaks, why it matters, how to reproduce it, and our workaround.';
const R2 = 'Rewrote it with the humanizer skill: no hype, no “load-bearing”, no emoji. Same facts, same workaround.';
const R4 = 'Made the step concrete: the retry should wait until 14:30:00 GMT, the date in the header.';
const ASK2 = 'make it sound like a human wrote it';
const ASK4 = 'say how long it should have waited';
const ITEMS = [
  { at: T.launch, type: 'notice', text: 'Continues the Claude Code session (14 earlier messages)' },
  { at: T.launch, type: 'user', html: `${esc(FIRST)}\n\nWhat to write: <span class="ul">${esc(NOTE)}</span>` },
  { at: T.read1, type: 'tool', tool: 'read', target: 'draft.md', done: T.read1 + .35, ms: 2 },
  { at: T.write1, type: 'tool', tool: 'write', target: 'draft.md', done: T.d1, added: 23, ms: 4 },
  { at: T.reply1[0], type: 'assistant', text: R1, stream: T.reply1 },
  { at: T.send2, type: 'user', text: ASK2 },
  { at: T.read2, type: 'tool', tool: 'read', target: 'skills/humanizer/SKILL.md', done: T.read2 + .4, ms: 3 },
  { at: T.edit2, type: 'tool', tool: 'edit', target: 'draft.md', done: T.d2, from: D1, to: D2, ms: 7 },
  { at: T.reply2[0], type: 'assistant', text: R2, stream: T.reply2 },
  { at: T.send4, type: 'user', text: ASK4, quote: { text: STEP, startLine: lineOf(D3a, D3a.indexOf(STEP)), endLine: lineOf(D3a, D3a.indexOf(STEP)) } },
  { at: T.edit4, type: 'tool', tool: 'edit', target: 'draft.md', done: T.d4, from: D3a, to: D4, ms: 6 },
  { at: T.reply4[0], type: 'assistant', text: R4, stream: T.reply4 },
];
// Agent runs: the editor is read-only and the send button stops the run.
const RUNS = [[T.launch, T.reply1[1]], [T.send2, T.reply2[1]], [T.send4, T.reply4[1]]];
const runAt = (t) => RUNS.find(([a, b]) => t >= a && t < b);

// ---------- Terminal ----------
const CMD1 = '/draftmux issue on retrykit for this', CMD5 = 'done, file it';
const SLASH = [['/doctor', 'Diagnose and verify your Claude Code installation'], ['/draftmux', 'Hand a GitHub, Slack or email message off to the Draftmux app'],
  ['/diff', 'View uncommitted changes and per-turn diffs']];
const TERM = [
  // Earlier in the session, long enough to fill the terminal from the top.
  { at: -1, bullet: 'ok', html: `<span class="tool">Bash</span>(cargo run --example sync -- --server staging)` },
  { at: -1, html: `<span class="b"></span><span class="mut">⎿</span>  <span class="out">synced 1,204 records in 3.8s</span>` },
  { at: -1, gap: true, bullet: 'fg', html: `<span class="fg">Sync works against staging again.</span>` },
  // The bug the issue reports.
  { at: -1, gap: true, cls: 'user', html: `<span class="mut">&gt;</span> <span class="fg">why does sync hammer the server when we get rate limited?</span>` },
  { at: -1, gap: true, bullet: 'ok', html: `<span class="tool">Read</span>(logs/sync.log)` },
  { at: -1, html: `<span class="b"></span><span class="mut">⎿</span>  <span class="out">40 retries/s from one client, each 429 with Retry-After: Wed, 11 Nov 2026 14:30:00 GMT</span>` },
  { at: -1, gap: true, bullet: 'ok', html: `<span class="tool">Search</span>(pattern: "Retry-After", path: "~/.cargo/registry/src/retrykit-0.7.2")` },
  { at: -1, html: `<span class="b"></span><span class="mut">⎿</span>  <span class="out">Found 1 file: src/policy.rs. parse_retry_after reads seconds only</span>` },
  { at: -1, gap: true, bullet: 'ok', html: `<span class="tool">Bash</span>(cargo test)` },
  { at: -1, html: `<span class="b"></span><span class="mut">⎿</span>  <span class="out">test result: ok. 42 passed; 0 failed</span>` },
  { at: -1, gap: true, bullet: 'fg', html: `<span class="fg">Fixed on our side. The bug is in retrykit 0.7: it reads Retry-After as seconds only, so a date falls back to no delay at all.</span>` },
  { at: T.enter1, gap: true, cls: 'user', html: `<span class="mut">&gt;</span> <span class="fg">${CMD1}</span>` },
  { at: T.bash1, id: 'note', gap: true, bullet: 'run', done: T.bash1Done, html: `<span class="tool">Bash</span>(draftmux open "$f" --target github --note <span class="note">'${NOTE}'</span>)` },
  { at: T.bash1Done, html: `<span class="b"></span><span class="mut">⎿</span>  <span class="out">01a1c900-4f2e-7b51-9c3e-2d7a1f0e8b61</span>` },
  { at: T.opened, gap: true, bullet: 'fg', html: `<span class="fg">Opened draft 01a1c900 in Draftmux. It's writing the first draft there; tell me when it's done.</span>` },
  { at: T.enter5, gap: true, cls: 'user', html: `<span class="mut">&gt;</span> <span class="fg">${CMD5}</span>` },
  { at: T.show5, gap: true, bullet: 'run', done: T.show5Done, html: `<span class="tool">Bash</span>(draftmux show --source "$f" --json)` },
  { at: T.show5Done, html: `<span class="b"></span><span class="mut">⎿</span>  <span class="out">{"fields":{"title":"Retry-After is ignored when it's an HTTP date"},"body":"## Description…</span>` },
  { at: T.gh5, gap: true, bullet: 'run', done: T.url5, html: `<span class="tool">Bash</span>(jq -j .body &lt;&lt;&lt; "$draft" | gh issue create --repo retrykit/retrykit --title "$title" --body-file -)` },
  { at: T.url5, id: 'url', html: `<span class="b"></span><span class="mut">⎿</span>  <span class="url">https://github.com/retrykit/retrykit/issues/214</span>` },
  { at: T.done5, gap: true, bullet: 'fg', html: `<span class="fg">Filed retrykit#214 with your draft, exactly as written.</span>` },
];
// The agent is working (spinner line) between a prompt and its last reply.
const TBUSY = [[T.enter1, T.opened], [T.enter5, T.done5]];
const SPIN = ['·', '✢', '✳', '✶', '✻', '✽'];

// ---------- Captions ----------
// [from, to, text, tag, sub]
const CAPTIONS = [
  [0, 7.3, 'Hand a message off from your agent session', '/draftmux'],
  [7.5, 10.5, 'Draftmux forks your session into a pi agent'],
  [10.7, 17.2, 'Agent writes the first draft from your session'],
  [17.4, 21.6, 'Ask for changes in the chat'],
  [21.8, 24.1, 'Pi uses your skills, like humanizer'],
  [24.3, 28.1, 'Edit the Markdown beside the preview', '⌥⌘E'],
  [28.3, 32.1, 'Continue the sentence at the cursor', '⌘↵'],
  [32.7, 37.9, 'Preview as GitHub, Slack, rich text or plain text'],
  [38.1, 43.1, 'Select text for alternatives'],
  [43.3, 48.1, 'Or select a part and say what to change'],
  [48.2, 51.1, 'Only that part changes'],
  [51.4, 57.2, 'Review every edit side by side', '⌘⇧E'],
  [57.4, T.outro, 'Back in the session, file it as written', null],
];
// The video cut's first caption waits for the title card to lift.
if (VIDEO) CAPTIONS[0][0] = TITLE[1];
const MODS = {
  '⌘': '<path d="M15 6v12a3 3 0 1 0 3-3H6a3 3 0 1 0 3 3V6a3 3 0 1 0-3 3h12a3 3 0 1 0-3-3"/>',
  '⇧': '<path d="M8.5 21v-7H4l8-10 8 10h-4.5v7Z"/>',
  '⌥': '<path d="M3 5h5.5l7 14H21M14 5h7"/>',
};
const modIcons = (s) => s.replace(/[⌘⇧⌥]/g, (m) => `<svg class="mod" viewBox="0 0 24 24" aria-hidden="true">${MODS[m]}</svg>`);
const KEYS = [[T.enter1 - .2, T.enter1 + .4, ['↵']], [T.toggle - .3, T.toggle + .3, ['⌥', '⌘', 'E']],
  [T.cont - .2, T.cont + .4, ['⌘', '↵']], [T.accept - .2, T.accept + .4, ['⇥']], [T.send2 - .2, T.send2 + .4, ['↵']],
  [T.send4 - .2, T.send4 + .4, ['↵']], [T.review - .3, T.review + .3, ['⌘', '⇧', 'E']],
  [T.enter5 - .2, T.enter5 + .4, ['↵']]];

// ---------- Build the Draftmux window ----------
const dmx = $('dmx');
dmx.innerHTML = `<div class="traffic"><i></i><i></i><i></i></div><div class="main" id="main">
  <div class="toolbar"><div class="tb-left"></div><div class="tb-main" id="tb"></div></div>
  <div class="body-row">
    <div class="sidebar" id="sb"><div class="sb-head">Drafts</div><div class="sb-listwrap"><div class="sb-list" id="sbl"></div></div></div>
    <div class="content" id="content">
      <div class="pane editor-pane" id="ed"><div class="editor-scroll scroll"><div class="editor-inner" id="edin"><div class="sel-layer" id="sel"></div><pre class="editor-flash" id="flash"></pre><pre class="editor-hl" id="hl"></pre></div></div></div>
      <div class="pv-wrap" id="pvwrap"><div class="pane preview-pane" id="pv"><div class="preview-scroll scroll" id="pvscroll"><div class="preview-body" id="pvbody"></div><div class="sel-layer psel" id="psel"></div><div class="sel-layer tics" id="tics"></div></div></div></div>
      <div class="pane rv-pane" id="rv"><div class="rv-bar" id="rvbar"></div><div class="rv-cols">
        ${['before', 'now'].map((side) => `<div class="rv-col ${side}"><div class="rv-head"><i class="rv-dot"></i><span class="trunc" id="rvh-${side}"></span></div>
          <div class="rv-body"><div class="preview-scroll scroll" id="rvs-${side}"><div class="preview-body" id="rvb-${side}"></div></div></div></div>`).join('')}</div></div>
    </div>
    <div class="chat-col"><div class="chat-head"><div class="agent">${I.pi}<span>Pi</span></div></div>
      <div class="chat-log"><div class="transcript-wrap"><div class="transcript scroll" id="tr"><div class="tfill"></div></div></div></div>
      <div class="composer"><div class="composer-box" id="cbox">
        <div class="input-wrap"><div class="ctext" id="ctext"></div><div class="placeholder" id="ph"></div></div>
        <div class="composer-foot"><div class="model"><div class="model-label"><span class="mname trunc">π anthropic/claude-sonnet-5-5</span><span class="mthink"> · thinking medium</span></div></div>
          <span id="cfoot"></span><button class="send" id="send"></button></div></div></div>
    </div>
  </div></div><div id="ov"><div class="ring" id="ring"></div><div class="popover" id="pop"></div><div class="menu rv-menu" id="rvmenu"></div></div>`;
const tr = $('tr');
const VERBS = { read: ['Reading', 'Read'], edit: ['Editing', 'Edited'], write: ['Writing', 'Wrote'] };
const spinner = (t) => `<span class="spinner"><span class="tr">${I['spinner-track']}</span><span class="arc" style="position:absolute;inset:0;display:block;transform:rotate(${Math.round(t * 514) % 360}deg)">${I['spinner-arc']}</span></span>`;
const quoteLabel = (q) => {
  let ex = q.text.replace(/\s+/g, ' ').trim();
  if (ex.length > 80) ex = ex.slice(0, 80) + '…';
  const lines = q.startLine === q.endLine ? `Line ${q.startLine}` : `Lines ${q.startLine}–${q.endLine}`;
  return `<div class="quote">${I.quote}<span class="q-text trunc">${esc(ex)}</span><span class="q-lines">${lines}</span></div>`;
};
for (const it of ITEMS) {
  it.el = el(`<div class="item${it.type === 'tool' ? ' tool' : ''}"></div>`);
  if (it.type === 'notice') it.el.innerHTML = `<div class="notice">${esc(it.text)}</div>`;
  if (it.type === 'user') it.el.innerHTML = `<div class="user-row"><div class="bubble">${it.quote ? `<div class="quotes">${quoteLabel(it.quote)}</div>` : ''}${it.html ?? esc(it.text)}</div></div>`;
  // An edit row counts the word changes in its diff (edit_changes.rs).
  if (it.from != null) it.changes = window.editChanges.editHunks(window.editChanges.lineDiff(it.from, it.to).lines).reduce((n, hk) => n + hk.changes.length, 0);
  tr.appendChild(it.el);
}
const activity = el('<div class="activity"></div>');
tr.appendChild(activity);

function toolHtml(it, t) {
  const running = t < it.done, [ing, ed] = VERBS[it.tool];
  const count = it.changes == null ? '' : `<span class="changes-n">${it.changes === 1 ? '1 change' : `${it.changes} changes`}</span>`;
  const stats = running ? '' : `${count}${it.added != null ? `<span class="stat add">+${it.added}</span>` : ''}<span class="dur">${it.ms}ms</span>${I.chevron.replace('<svg', '<svg class="chev"')}`;
  const settle = running ? '' : ` style="opacity:${easeOut((t - it.done) / .15).toFixed(3)}"`;
  return `<div class="tool-row"><span class="tool-ic"${settle}>${running ? spinner(t) : I['tool-' + it.tool]}</span>
    <span class="tool-txt"><span class="tool-verb">${running ? ing : ed}</span><span class="tool-target trunc">${it.target}</span></span>
    <span class="tool-meta"${settle}>${stats}</span></div>`;
}
// The activity label's shimmer: a highlight sweeps across the letters every 1.8 s.
function shimmerHtml(text, t) {
  return [...text].map((c, i) => {
    const p = ((((t * 1000 - i * 45) % 1800) + 1800) % 1800) / 1800;
    const k = p < .15 ? ease(p / .15) : p < .4 ? 1 - ease((p - .15) / .25) : 0;
    return `<span style="color:color-mix(in srgb, var(--text) ${Math.round(k * 100)}%, var(--shimmer-base))">${esc(c)}</span>`;
  }).join('');
}
// Streams by whole words, so markdown markers never show half-open.
function streamed(text, t, [a, b]) {
  const words = text.split(' '), n = Math.ceil(words.length * clamp((t - a) / (b - a)));
  let s = words.slice(0, n).join(' ');
  if ((s.match(/\*\*/g) || []).length % 2) s += '**';
  return s;
}

// A reply's newest words fade in over the last few word steps, so the text
// arrives as a soft wave rather than word by word. Words stream over the
// first 65% of a reply's window.
const FADE_WORDS = 4, STREAM_SPAN = .65;
function replyAt(it, t) {
  const a = it.stream[0], b = a + (it.stream[1] - a) * STREAM_SPAN;
  const pos = it.text.split(' ').length * Math.max(0, (t - a) / (b - a));
  const html = `<div class="asst">${MD.renderChat(streamed(it.text, t, [a, b]))}</div>`;
  if (it.el.dataset.html !== html) {
    setHtml(it.el, html);
    const walk = document.createTreeWalker(it.el, NodeFilter.SHOW_TEXT), nodes = [];
    while (walk.nextNode()) nodes.push(walk.currentNode);
    for (const n of nodes) {
      const f = document.createDocumentFragment();
      for (const w of n.data.split(/(\s+)/)) {
        if (!w) continue;
        if (/^\s+$/.test(w)) { f.append(w); continue; }
        const e = document.createElement('span'); e.className = 'w'; e.textContent = w; f.append(e);
      }
      n.replaceWith(f);
    }
    it.words = [...it.el.querySelectorAll('span.w')];
  }
  const n = Math.ceil(Math.min(pos, it.text.split(' ').length)), m = it.words.length;
  it.words.forEach((w, i) => { w.style.opacity = clamp((pos - (n - m + i)) / FADE_WORDS); });
}

function chatAt(t) {
  for (const it of ITEMS) {
    const on = t >= it.at;
    it.el.style.display = on ? '' : 'none';
    if (!on) continue;
    const p = easeOut((t - it.at) / .25);
    it.el.style.opacity = it.type === 'assistant' ? 1 : p;
    it.el.style.transform = it.type === 'assistant' ? '' : `translateY(${(1 - p) * 6}px)`;
    it.el.classList.toggle('running', it.type === 'tool' && t < it.done);
    if (it.type === 'tool') setHtml(it.el, toolHtml(it, t));
    if (it.type === 'assistant') replyAt(it, t);
    const ul = it.el.querySelector('.ul');
    if (ul) ul.style.textDecorationColor = `rgba(255,255,255,${.9 * win(t, T.launch + .4, T.note[1] + .4, .3)})`;
  }
  // "Thinking…" while a run waits on the model: no tool running, no reply yet.
  const run = runAt(t);
  const busyTool = ITEMS.some((it) => it.type === 'tool' && t >= it.at && t < it.done);
  const replying = ITEMS.some((it) => it.type === 'assistant' && run && it.at >= run[0] && it.at <= t);
  const thinking = run && !busyTool && !replying && t >= run[0] + .15;
  activity.style.display = thinking ? '' : 'none';
  if (thinking) activity.innerHTML = shimmerHtml('Thinking…', t);
  tr.scrollTop = tr.scrollHeight;
}

// Typed text in the composer: [from, to, text].
const COMPOSE = [[T.type2[0], T.type2[1], T.send2, ASK2], [T.type4[0], T.type4[1], T.send4, ASK4]];
function composerAt(t, text) {
  const c = COMPOSE.find(([a, , s]) => t >= a - .4 && t < s);
  const value = c ? typed(c[3], t, c[0], c[1]) : '';
  const focus = t >= T.click2 && t < T.send2;
  setHtml($('ctext'), esc(value) + (focus && (value || Math.floor(t * 2.2) % 2 === 0) ? '<span class="ccaret"></span>' : ''));
  const chip = chipAt(t, text);
  $('ph').textContent = value ? '' : chip ? 'Ask pi to change the selection, or ask about it…' : 'Ask pi to change the draft, or ask about the session…';
  const busy = !!runAt(t);
  // The attached selection shows as a chip in the footer, in place of the
  // send hint; the hint also hides while the agent runs.
  const rise = easeOut((t - CHIP_IN) / .2);
  setHtml($('cfoot'), chip ? `<span class="qwrap" style="opacity:${rise.toFixed(3)}"><button class="qchip">${I.quote}1 selection</button></span>` : busy ? '' : '<span class="hint">↵ send · ⇧↵ newline</span>');
  $('cfoot').style.cssText = 'display:contents';
  // A Claude run reports the model it uses as it starts.
  const send = $('send');
  send.className = 'send' + (busy ? ' busy' : value ? '' : ' disabled');
  setHtml(send, busy ? I.stop : I.send);
  $('cbox').style.borderColor = focus ? 'var(--accent)' : '';
}

const TARGETS = [['github', 'GitHub'], ['slack', 'Slack'], ['rich', 'Rich text'], ['plain', 'Plain text']];
function toolbarAt(t, text, editor, target) {
  const title = text ? MD.draftTitle(text, { emoji: false }) : 'Issue on retrykit';
  setHtml($('tb'), `<button class="icon-btn" id="b-sidebar">${I.sidebar}</button>
    <div class="tb-title trunc">${esc(title)}</div><div class="tb-sub trunc">~/code/tidepool</div><div class="spacer"></div>
    <div class="tb-actions">${changesButton(t, text)}<div class="format-wrap">${target === 'slack' ? '' : `<div class="format-group${runAt(t) ? ' disabled' : ''}"><button class="format-btn">Format <span class="kbd">⌘⇧F</span></button><button class="format-chev">${I.chevron}</button></div>`}</div>
      <button class="icon-btn pane-toggle${editor ? '' : ' off'}" id="b-editor">${I['editor-pane']}</button>
      <button class="icon-btn pane-toggle" id="b-preview">${I.preview}</button>
      <div class="segments" id="segs">${TARGETS.map(([k, l]) => `<button class="segment${k === 'github' ? ' gh' : ''}${k === target ? ' on' : ''}" id="seg-${k}">${l}${k === 'github' ? I.chevron.replace('<svg', '<svg class="seg-chev"') : ''}</button>`).join('')}</div>
      <button class="copy-btn">${I.copy}Copy <span class="kbd">⌘⇧C</span></button></div>`);
}

// Offsets in the draft -> a Range over the highlighted mirror's text.
function textRange(root, start, end) {
  const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const r = document.createRange();
  let n, pos = 0, set = false;
  while ((n = w.nextNode())) {
    const len = n.data.length;
    if (!set && start <= pos + len) { r.setStart(n, start - pos); set = true; }
    if (set && end <= pos + len) { r.setEnd(n, end - pos); return r; }
    pos += len;
  }
  return r;
}
// The first text node range in root containing needle.
function findRange(root, needle) {
  const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let n;
  while ((n = w.nextNode())) { const i = n.data.indexOf(needle); if (i >= 0) { const r = document.createRange(); r.setStart(n, i); r.setEnd(n, i + needle.length); return r; } }
  return null;
}
// A client rect in el's own (untransformed) coordinates.
function rel(r, e) {
  const b = e.getBoundingClientRect(), k = b.width / e.offsetWidth;
  return { x: (r.left - b.left) / k, y: (r.top - b.top) / k, w: r.width / k, h: r.height / k };
}
const union = (a, b) => { const x = Math.min(a.x, b.x), y = Math.min(a.y, b.y); return { x, y, w: Math.max(a.x + a.w, b.x + b.w) - x, h: Math.max(a.y + a.h, b.y + b.h) - y }; };
const pad = (r, px, py = px) => ({ x: r.x - px, y: r.y - py, w: r.w + 2 * px, h: r.h + 2 * py });
const world = $('world');
const W = (e) => rel(e.getBoundingClientRect(), world);
const WR = (r) => rel(r, world);

// The preview's list item for the step the comment changes.
function stepItem() {
  return [...$('pvbody').querySelectorAll('li')].find((x) => x.textContent.startsWith('The retry fires'));
}

// The drafts already open when the hand-off arrives: [title, updated, cwd].
const OTHER_DRAFTS = [
  ['Reply to Mika about the sync outage', '25m', '~/code/tidepool'],
  ['Release notes for 0.9', '2h', '~/code/tidepool'],
  ['Add offline mode to the mobile client', 'Tue', '~/code/tidepool-mobile'],
  ['Self-hosting section for the README', 'Mon', '~/code/harbor'],
];
const sbRow = (title, time, cwd, selected) => `<div class="row${selected ? ' selected' : ''}"><div class="row-col">
  <div class="row-l1"><div class="row-title trunc">${esc(title)}</div><div class="row-time">${time}</div></div>
  <div class="row-l2"><span class="row-cwd trunc">${cwd}</span></div></div></div>`;
// The sidebar lists the other drafts; the hand-off's draft slides in on top,
// selected, and ⌘\ hides the sidebar before the camera moves in.
function sidebarAt(t, title) {
  const open = 1 - ease((t - T.sbHide) / .35), k = ease((t - T.sbIn) / .4);
  $('main').classList.toggle('no-sidebar', open <= 0);
  $('sb').style.display = open > 0 ? '' : 'none';
  $('sb').style.width = `${Math.round(228 * open)}px`;
  $('sbl').style.transform = `translateX(${(228 * (open - 1)).toFixed(1)}px)`;
  document.querySelector('#dmx .tb-left').style.width = open > 0 ? `${Math.round(100 + 128 * open)}px` : '';
  if (open <= 0) return;
  setHtml($('sbl'), `<div class="sb-new">${sbRow(title, 'now', '~/code/tidepool', true)}</div>`
    + OTHER_DRAFTS.map(([a, b, c], i) => sbRow(a, b, c, k < .5 && i === 0)).join(''));
  const row = $('sbl').firstElementChild;
  Object.assign(row.style, { height: `${(row.scrollHeight * k).toFixed(2)}px`, overflow: 'hidden', opacity: k, flex: 'none' });
}

let lastHl = null;
function draftmuxAt(ct) {
  const text = draftAt(ct);
  const target = ct >= T.slack && ct < T.rich ? 'slack' : ct >= T.rich && ct < T.github ? 'rich' : 'github';
  toolbarAt(ct, text, ct >= T.toggle, target);
  sidebarAt(ct, text ? MD.draftTitle(text, { emoji: false }) : 'Issue on retrykit');
  // Editor.
  const hl = $('hl');
  // The ghost is shaped into its line, unhighlighted, after the cursor.
  const ghost = ghostAt(ct), hlKey = text + '\0' + (ghost ? ghost.text : '');
  if (lastHl !== hlKey) {
    hl.innerHTML = MD.highlightSource(text) + '\n ';
    if (ghost?.text) textRange(hl, ghost.at, ghost.at).insertNode(el(`<span class="ghost">${esc(ghost.text)}</span>`));
    lastHl = hlKey;
  }
  $('ed').classList.toggle('read-only', !!runAt(ct));
  const cascade = clamp((ct - (ct >= T.d2 ? T.d2 : T.d1)) / .5);
  const mask = text && cascade < 1 ? `linear-gradient(#000 ${cascade * 100}%, transparent ${cascade * 100 + 12}%)` : '';
  hl.style.webkitMaskImage = mask; $('pvbody').style.webkitMaskImage = mask;
  // The draft opens in the preview alone; the editor slides in from the left
  // at its full width, so its lines don't rewrap mid-move.
  const cw = $('content').offsetWidth, half = Math.round(cw / 2);
  const ew = Math.round(half * ease((ct - T.toggle) / .4));
  const ed = $('ed');
  ed.style.width = `${ew}px`; ed.style.display = ew > 0 ? '' : 'none';
  ed.firstElementChild.style.right = 'auto'; ed.firstElementChild.style.width = `${half}px`;
  $('pvwrap').style.width = `${cw - ew}px`;
  $('pvwrap').style.borderLeftWidth = ew > 0 ? '' : '0';
  $('pv').style.width = `${cw - ew}px`;
  const { html } = MD.renderPreview(text, target);
  // An empty draft shows the app's skeleton of the message while pi writes it.
  setHtml($('pvbody'), text ? html : skeletonHtml(ct));
  const md = $('pvbody').querySelector('.md');
  $('pvscroll').style.background = md ? getComputedStyle(md).getPropertyValue('--md-bg') : '';
  // Selection highlight, drawn behind the mirror.
  const sel = selectionAt(ct, text), layer = $('sel');
  let rects = [];
  if (sel && sel[1] > sel[0]) rects = [...textRange(hl, sel[0], sel[1]).getClientRects()].map((r) => rel(r, $('edin')));
  // The caret after a click in the Markdown, until the camera leaves it:
  // first in the step the version is typed into, then at the end of the
  // description, where the continuation goes.
  if (ct >= T.mdClick && ct < T.targets[0]) {
    const cont = ct >= T.contClick;
    const at = cont ? CONT_AT + typed(CONT_TYPED, ct, ...T.typeCont).length + (ct >= T.accept ? CONT_GHOST.length : 0) : MD_AT + typed(MD_ADD, ct, ...T.typeMd).length;
    const typing = cont ? (ct >= T.typeCont[0] && ct < T.typeCont[1] + .3) || (ct >= T.accept && ct < T.accept + .3) : ct >= T.typeMd[0] && ct < T.typeMd[1] + .3;
    const r = rel(textRange(hl, at - 1, at).getBoundingClientRect(), $('edin'));
    if (typing || Math.floor((ct - (cont ? T.contClick : T.mdClick)) * 2.2) % 2 === 0) rects.push({ x: r.x + r.w, y: r.y, w: 2, h: r.h, caret: true });
    // Three dots after the caret while the continuation waits on the agent.
    if (ghost && !ghost.text) {
      const em = 12.5, d = em * .32;
      for (let i = 0; i < 3; i++) {
        const lift = dotLift(ct - ghost.since, i);
        rects.push({ x: r.x + r.w + em * .48 + (d + em * .24) * i, y: r.y + (r.h - d) / 2 - em * .16 * lift, w: d, h: d, dot: .3 + .7 * lift });
      }
    }
  }
  setHtml(layer, selHtml(rects));
  const ps = previewSelAt(ct);
  layer.classList.toggle('inactive', !!sel && (!sel[2] || !!ps));
  const pr = ps && ps[1] > 0 && previewRange(ps[0], ps[1]);
  const prects = pr ? [...pr.getClientRects()].map((r) => rel(r, $('pvscroll'))) : [];
  setHtml($('psel'), selHtml(prects));
  $('psel').classList.toggle('inactive', !!ps && !ps[2]);
  ticsAt(ct);
  flashAt(ct, text);
  chatAt(ct);
  composerAt(ct, text);
  popoverAt(ct, text, sel);
  ringAt(ct);
  reviewAt(ct, text);
}

// Selection rects in the pane's own pixels. Measuring through the camera's
// scale leaves sub-pixel noise that changes as the camera drifts; snapping it
// keeps a still selection from creeping against its text.
const selHtml = (rects) => rects.map((r) => {
  const x = Math.round(r.x), y = Math.round(r.y);
  if (r.caret) return `<i class="caret" style="left:${x}px;top:${y}px;width:2px;height:${Math.round(r.h)}px"></i>`;
  if (r.dot) return `<i class="gdot" style="left:${r.x.toFixed(2)}px;top:${r.y.toFixed(2)}px;width:${r.w}px;height:${r.h}px;opacity:${r.dot.toFixed(3)}"></i>`;
  return `<i style="left:${x}px;top:${y}px;width:${Math.round(r.x + r.w) - x}px;height:${Math.round(r.y + r.h) - y}px"></i>`;
}).join('');

// Text an edit changed gets a tint that fades by the square of its progress
// over 900 ms, as the app's Editor::flash paints it. Applying an alternative
// flashes the whole inserted option, as the selection popover does; an agent
// edit flashes only the changed ranges. [at, before, after, inserted option].
const inserted = (text, option) => { const a = text.indexOf(option); return [[a, a + option.length]]; };
const FLASHES = [[T.apply3, D2e, D3a, S2_NEW], [T.d4, D3a, D4]];
function flashAt(t, text) {
  const f = FLASHES.find(([a]) => t >= a && t < a + .9), layer = $('flash');
  layer.style.display = f ? '' : 'none';
  if (!f) return;
  let html = '', at = 0;
  for (const [a, b] of f[3] ? inserted(f[2], f[3]) : window.changedRanges(f[1], f[2])) { html += esc(text.slice(at, a)) + `<mark>${esc(text.slice(a, b))}</mark>`; at = b; }
  setHtml(layer, html + esc(text.slice(at)) + '\n ');
  const p = (t - f[0]) / .9;
  layer.style.opacity = 1 - p * p;
}

// ---------- Selection popover ----------
// selection_popover.rs: the actions for a selection, the options the agent
// streams in, or the comment field.
const ALTS = ['Every rate-limited client retries in a tight loop.', S2_NEW, 'Under a rate limit, this turns into a retry storm.'];
const ACTIONS = ['Alternatives', 'Shorten', 'Tighten'], COMMENT = ACTIONS.length;
const AGENT = 'pi';
// Options stream in muted, one at a time, after a short wait.
const PREVIEW_AT = .3, PREVIEW_GAP = .2;
function popoverState(t) {
  // at: when the popover appeared; since: when this mode started; hl: the
  // highlighted row and hlAt when it landed; out: when it is dismissed.
  const st = { mode: null, at: 0, since: 0, hl: -1, hlAt: 0, out: Infinity, alts: ALTS, label: 'Generating alternatives…' };
  const pass = (pop, alt, opts, apply, item, hlOpt, hlOptAt) => {
    if (t < pop || t >= apply) return false;
    st.at = pop;
    if (t < alt) Object.assign(st, { mode: 'actions', since: pop, hl: t >= alt - .3 ? item : -1, hlAt: alt - .3 });
    else if (t < opts) Object.assign(st, { mode: 'running', since: alt, previews: Math.min(st.alts.length, Math.floor((t - alt - PREVIEW_AT) / PREVIEW_GAP) + 1) });
    else Object.assign(st, { mode: 'results', since: alt, hl: hlOpt, hlAt: hlOptAt });
    return true;
  };
  if (pass(T.pop3, T.alt3, T.opts3, T.apply3, 0, t >= T.hover3 ? 1 : 0, t >= T.hover3 ? T.hover3 : T.opts3)) return st;
  // Comment: the field takes the popover's place, and sending closes it.
  const pop4 = T.pop4;
  if (t >= pop4 && t < T.send4 + .2) {
    Object.assign(st, { at: pop4, out: T.send4 });
    if (t < T.comment4) Object.assign(st, { mode: 'actions', since: pop4, hl: t >= T.comment4 - .25 ? COMMENT : -1, hlAt: T.comment4 - .25 });
    else Object.assign(st, { mode: 'comment', since: T.comment4 });
  }
  return st;
}
function popoverHtml(st, t) {
  const hlStyle = ` style="background-color:color-mix(in srgb, var(--accent) ${Math.round(easeOut((t - st.hlAt) / .12) * 100)}%, transparent)"`;
  const item = (i, inner) => `<div class="pop-item${st.hl === i ? ' hl' : ''}"${st.hl === i ? hlStyle : ''}>${inner}</div>`;
  const opt = (o, i, live) => `<div class="pop-opt${live && st.hl === i ? ' hl' : ''}"${live && st.hl === i ? hlStyle : ''}><span class="n">${i + 1}</span><span class="o">${esc(o)}</span></div>`;
  if (st.mode === 'actions') {
    return `<div class="pop-body"><div class="pop-actions">${ACTIONS.map((a, i) => item(i, `${i === 0 ? '<span class="spark">✦</span>' : ''}<span class="trunc" style="max-width:140px">${a}</span>`)).join('')}`
      // The app adds a "⌘-drag to add" hint here until the first added
      // selection; the video leaves it out so the row fits the camera's shots.
      + `<div class="pop-div"></div>${item(COMMENT, `${COMMENT_ICON}<span>Comment</span>`)}</div></div>`;
  }
  if (st.mode === 'running' && st.previews < 1) return `<div class="pop-body"><div class="pop-running">${spinner(t)}<span>${esc(st.label)}</span></div></div>`;
  if (st.mode === 'running') {
    return `<div class="pop-body"><div class="pop-results previews">${st.alts.slice(0, st.previews).map((o, i) => opt(o, i, false)).join('')}
      <div class="pop-foot run">${spinner(t)}<span>${esc(st.label)}</span><span class="esc">esc cancel</span></div></div></div>`;
  }
  if (st.mode === 'results') {
    return `<div class="pop-body"><div class="pop-results">${st.alts.map((o, i) => opt(o, i, true)).join('')}
      <div class="pop-foot"><span>1–${st.alts.length} or ↵ apply</span><span>↑↓ move</span><span>esc cancel</span></div></div></div>`;
  }
  const value = typed(ASK4, t, ...T.type4), caret = value || Math.floor(t * 2.2) % 2 === 0 ? '<span class="ccaret"></span>' : '';
  return `<div class="pop-body"><div class="pop-comment"><div class="pop-field"><div class="pop-input"><div class="pop-ctext">${esc(value)}${caret}</div>
    ${value ? '' : `<div class="pop-ph">Tell ${AGENT} what to change here…</div>`}</div><div class="pop-send${value ? '' : ' disabled'}">${I.send}</div></div>
    <div class="pop-hints"><span>↵ send to ${AGENT}</span><span>⇧↵ newline</span><span>esc back</span></div></div></div>`;
}
const COMMENT_ICON = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"><path d="M2.75 4.25a1.5 1.5 0 0 1 1.5-1.5h7.5a1.5 1.5 0 0 1 1.5 1.5v5a1.5 1.5 0 0 1-1.5 1.5H7.5l-3 2.5v-2.5h-.25a1.5 1.5 0 0 1-1.5-1.5z"/></svg>';
// The tallest the popover gets for a selection: its options list, measured
// once, so it picks the side that fits them and stays there.
const popNeed = new Map();
function popoverAt(t, text, sel) {
  const pop = $('pop'), st = popoverState(t);
  pop.style.display = st.mode ? '' : 'none';
  if (!st.mode) return;
  if (!popNeed.has(st.at)) {
    setHtml(pop, popoverHtml({ ...st, mode: st.at === T.pop4 ? 'comment' : 'results' }, t));
    popNeed.set(st.at, pop.offsetHeight);
  }
  setHtml(pop, popoverHtml(st, t));
  const k = easeOut((t - st.at) / .12) * (1 - ease((t - st.out) / .2));
  pop.style.opacity = k; pop.style.transform = `scale(${.97 + .03 * k})`;
  if (st.mode !== 'actions') pop.firstElementChild.style.opacity = easeOut((t - st.since) / .16);
  // Above the selection's first line in the preview, or below its last when
  // the options wouldn't fit above.
  const range = previewRange(...previewSelAt(t).slice(0, 2));
  const rs = [...range.getClientRects()].map((r) => rel(r, dmx));
  const first = rs[0], last = rs[rs.length - 1];
  const pane = rel($('pv').getBoundingClientRect(), dmx);
  const w = pop.offsetWidth, h = pop.offsetHeight, need = Math.max(h, popNeed.get(st.at));
  const x = Math.max(pane.x + 8, Math.min(first.x - 6, pane.x + pane.w - w - 8));
  const above = first.y - 6 - need >= pane.y + 8;
  pop.style.left = `${x}px`; pop.style.top = `${above ? first.y - 6 - h : last.y + last.h + 6}px`;
}

// ---------- Review (chrome/review.rs) ----------
// Each agent turn that edits the draft, and each option applied from the
// popover, adds an edit to the draft's history: [when, label, text before].
// The menu's ages stand in for the minutes a real session takes.
const EDITS = [[T.d2, ASK2, D1, '4m'], [T.apply3, 'Alternatives', D2e, '2m'], [T.d4, ASK4, D3a, 'now']];
const editsAt = (t) => EDITS.filter(([at]) => t >= at);
const clip = (s, n) => [...s].length <= n ? s : [...s].slice(0, n - 1).join('') + '…';
// Review compares since the last edit until the range menu picks Alternatives.
const SINCE = 1;
// Done keeps every change.
const keptAt = (t) => t >= T.done ? Infinity : 0;
const cmpCache = new Map();
function comparison(base, text) {
  const key = base + '\0' + text;
  if (!cmpCache.has(key)) cmpCache.set(key, window.reviewDiff.compare(base, text));
  return cmpCache.get(key);
}
function reviewState(t, text) {
  const edits = editsAt(t);
  if (!edits.length) return null;
  const since = t >= T.since ? SINCE : edits.length - 1;
  const cmp = comparison(edits[since][2], text);
  // Kept changes are those of the comparison since Alternatives.
  const kept = since === SINCE ? Math.min(keptAt(t), cmp.hunks.length) : 0;
  return { edits, since, cmp, kept, open: t >= T.review && t < T.done, selected: null };
}
// The toolbar's Changes button, with the count not yet kept.
function changesButton(t, text) {
  const r = reviewState(t, text);
  if (!r) return '';
  const count = r.cmp.hunks.length - r.kept, fade = easeOut((t - r.edits[0][0]) / .2);
  return `<button class="rv-btn${r.open ? ' on' : ''}${count ? ' counted' : ''}" id="b-review" style="opacity:${fade.toFixed(3)}">${count ? `<span class="rv-count">${count}</span>` : ''}${count === 1 ? 'Change' : 'Changes'}</button>`;
}
const chev = (cls = '') => I.chevron.replace('<svg', `<svg class="rv-chev ${cls}"`);
let rvDrawn = null;
function reviewAt(t, text) {
  const r = reviewState(t, text), open = !!r && r.open;
  $('rv').style.display = open ? 'block' : 'none';
  $('ed').style.visibility = $('pvwrap').style.visibility = open ? 'hidden' : '';
  $('rvmenu').style.display = 'none';
  if (!open) return;
  const n = r.edits.length, last = r.since + 1 >= n, count = r.cmp.hunks.length;
  // The columns: each side's text with its changes marked.
  const key = r.edits[r.since][2] + '\0' + text;
  if (rvDrawn !== key) {
    rvDrawn = key;
    for (const [side, which] of [['before', 'before'], ['now', 'after']]) {
      const box = window.reviewDiff.renderSide(window.reviewDiff.marked(r.cmp, which), 'github');
      $(`rvb-${side}`).replaceChildren(...box.childNodes);
      const md = $(`rvb-${side}`).querySelector('.md');
      $(`rvs-${side}`).style.background = md ? getComputedStyle(md).getPropertyValue('--md-bg') : '';
    }
  }
  $('rvh-before').textContent = last ? `Before “${clip(r.edits[r.since][1], 30)}”` : `Before the last ${n - r.since} edits`;
  $('rvh-now').textContent = 'Now';
  const sel = r.selected, pos = sel != null ? `${sel + 1} of ${count}` : `– of ${count}`;
  const menuOpen = t >= T.range && t < T.since;
  setHtml($('rvbar'), `<button class="rv-range${menuOpen ? ' open' : ''}" id="rv-range">${last ? 'Last edit' : `Last ${n - r.since} edits`}${chev('down')}</button>
    <div class="rv-changes"><div class="rv-nav"><button class="rv-step">${chev('back')}</button><span class="rv-pos">${pos}</span><button class="rv-step">${chev()}</button></div>
      ${sel != null ? '<button class="rv-b push">Revert</button>' : '<button class="rv-b push">Restore</button>'}
      <button class="rv-b ${sel != null && sel < r.kept ? 'plain' : 'push'}${sel == null ? ' off' : ''}">${sel != null && sel < r.kept ? 'Kept' : 'Keep'}</button></div>
    <div class="rv-spacer"></div><button class="rv-done" id="rv-done">Done</button>`);
  for (const m of $('rv').querySelectorAll('.rl')) {
    const i = +m.dataset.h;
    m.classList.toggle('kept', i < r.kept);
    m.classList.toggle('sel', i === sel);
  }
  // The changes stepped through all sit in the columns' first screen, so
  // neither column scrolls.
  // The range menu, newest edit first, under its button.
  if (menuOpen) {
    const menu = $('rvmenu'), rows = r.edits.map(([, label, , age], i) => ({ label: `“${clip(label, 34)}”`, age, i })).reverse();
    const hl = t >= T.since - .35 ? rows.findIndex((x) => x.i === SINCE) : -1;
    setHtml(menu, `<div class="rv-menu-head">Show Changes Since</div>${rows.map((x, k) => `<div class="menu-item${k === hl ? ' hl' : ''}"><span class="chk">${x.i === r.since ? '✓' : ''}</span><span class="trunc">${esc(x.label)}</span><span class="rv-menu-detail">${x.age}</span></div>`).join('')}
      <div class="menu-sep"></div><div class="menu-item"><span class="chk"></span><span class="trunc">Restore the Before Column</span></div>`);
    const b = rel($('rv-range').getBoundingClientRect(), dmx);
    Object.assign(menu.style, { display: 'flex', left: `${b.x}px`, top: `${b.y + b.h + 4}px`, opacity: easeOut((t - T.range) / .1) });
  }
}

// Rings around what an edit changed in the preview: [from, to, rect in dmx].
const RINGS = [
  [T.ring4[0], T.ring4[1], () => pad(rel(stepItem().getBoundingClientRect(), dmx), 8, 4)],
];
function ringAt(t) {
  const ring = $('ring'), g = RINGS.find(([a, b]) => t >= a - .3 && t < b + .3);
  const on = g ? win(t, g[0], g[1], .3) : 0;
  const r = on > 0 && g[2]();
  ring.style.opacity = r ? on : 0;
  if (r) Object.assign(ring.style, { left: `${r.x - 8}px`, top: `${r.y}px`, height: `${r.h}px` });
}

// ---------- Terminal ----------
for (const l of TERM) {
  l.el = el(`<div class="tl${l.gap ? ' gap' : ''}${l.cls ? ' ' + l.cls : ''}">${l.bullet ? '<span class="b">⏺</span>' : ''}${l.html}</div>`);
  l.b = l.el.querySelector('.b');
  $('tbody').appendChild(l.el);
}
const spinLine = el('<div class="tl gap"></div>');
const tinput = el('<div class="tinput"><span class="pr">&gt;</span><span class="txt"></span></div>');
const tmenu = el('<div class="tmenu"></div>');
const thint = el('<div class="thint">? for shortcuts</div>');
// The menu and the hint share a footer as tall as the full menu, so the
// bottom-aligned transcript doesn't jump as the menu narrows while typing.
const tfoot = el('<div class="tfoot"></div>');
tfoot.append(tmenu, thint);
$('tbody').append(spinLine, tinput, tfoot);


function terminalAt(t) {
  const past = TERM.filter((l) => l.at < 0);
  for (const l of TERM) {
    l.el.style.display = t >= l.at ? '' : 'none';
    // Scrollback rows keep their place while hidden, so the prompt stays put
    // as they come in: each wipes in from the left, out of a blur, and rises.
    const i = past.indexOf(l);
    if (i >= 0) {
      const [a, b] = T.recall, dur = .5, at = a + i * (b - a - dur) / Math.max(1, past.length - 1);
      const p = clamp((t - at) / dur), e = easeOut(p);
      Object.assign(l.el.style, p >= 1 ? { opacity: '', filter: '', transform: '', maskImage: '' } : {
        opacity: e, filter: `blur(${(6 * (1 - e)).toFixed(2)}px)`, transform: `translateY(${(10 * (1 - e)).toFixed(1)}px)`,
        maskImage: `linear-gradient(90deg, #000 ${(e * 130 - 30).toFixed(1)}%, transparent ${(e * 130).toFixed(1)}%)` });
    }
    if (!l.b) continue;
    const running = l.done && t < l.done;
    l.b.className = 'b ' + (running ? 'mut' : l.bullet === 'run' ? 'ok' : l.bullet);
    l.b.style.opacity = running ? (Math.floor(t * 2.5) % 2 ? .25 : 1) : 1;
  }
  const note = TERM.find((l) => l.id === 'note').el.querySelector('.note');
  note.style.textDecorationColor = `rgba(122,166,255,${win(t, T.note[0], T.note[1], .3)})`;
  // Spinner line while the agent works.
  const busy = TBUSY.find(([a, b]) => t >= a && t < b);
  spinLine.style.display = busy ? '' : 'none';
  if (busy) {
    const s = Math.floor((t - busy[0]) * 1);
    setHtml(spinLine, `<span class="spin">${SPIN[Math.floor(t * 8) % SPIN.length]} Pondering…</span> <span class="mut">(${s}s · esc to interrupt)</span>`);
  }
  // Prompt typing.
  let value = '';
  if (t >= T.type1[0] && t < T.enter1) value = typed(CMD1, t, ...T.type1);
  if (t >= T.type5[0] && t < T.enter5) value = typed(CMD5, t, ...T.type5);
  setHtml(tinput.querySelector('.txt'), esc(value) + (Math.floor(t * 2.2) % 2 === 0 || value ? '<span class="tcaret"></span>' : ''));
  // Slash command menu while the command name is being typed.
  const cmd = value.startsWith('/') && !value.includes(' ') ? value : '';
  if (!tfoot.style.height && document.fonts.status === 'loaded') {
    setHtml(tmenu, SLASH.map(([c, d]) => `<div class="row"><span class="c">${c}</span><span>${d}</span></div>`).join(''));
    tfoot.style.height = `${tmenu.offsetHeight}px`;
  }
  const rows = cmd ? SLASH.filter(([c]) => c.startsWith(cmd)).slice(0, 3) : [];
  setHtml(tmenu, rows.map(([c, d]) => `<div class="row${c === '/draftmux' ? ' on' : ''}"><span class="c">${c}</span><span>${d}</span></div>`).join(''));
  thint.style.display = rows.length ? 'none' : '';
}

// ---------- Camera ----------
// Named shots, as world rects. The camera fits the rect into the frame.
// Every Draftmux shot has this height, so all fit at 2x and moves between
// them are pans. Scale changes only when the story changes window.
const WORK_H = 465;
// The desk shown beside a window edge that a shot reaches: enough that a
// hold's slow push doesn't carry the edge to the frame's.
const EDGE = 24;
const SHOTS = {
  desk: () => ({ x: 0, y: 0, w: 1920, h: 1080 }),
  // The whole Draftmux window, for orienting when it opens.
  // Its laid-out box, since the window may be hidden or mid-launch.
  dmx: () => ({ x: 440, y: 130, w: 1422, h: 800 }),
  // The whole terminal window, where the video cut opens.
  term: () => pad(W($('term')), 40),
  // Bottom of the terminal: the prompt and the output that appears above it.
  termTail: () => { const b = W($('term')); return { x: b.x - 20, y: b.y + b.h - 504, w: b.w + 40, h: 504 }; },
  // Top of editor and preview (the whole preview before the toggle), toolbar included.
  source: () => { const r = W($('content')); return { x: r.x - EDGE, y: W(dmx).y, w: r.w + EDGE + 10, h: WORK_H }; },
  // Preview and the target switch above it.
  targets: () => { const p = W($('pv')), g = W($('segs')); return { x: p.x - 10, y: W(dmx).y, w: g.x + g.w + 20 - p.x, h: WORK_H }; },
  // From just above Steps to reproduce, so a popover over the last step fits,
  // down through Workaround.
  steps: () => { const r = W($('content')), h = W([...$('pvbody').querySelectorAll('h2')].find((e) => /^Steps/.test(e.textContent)));
    return { x: r.x - EDGE, y: Math.min(h.y - 70, r.y + r.h - WORK_H), w: r.w + EDGE + 10, h: WORK_H }; },
  // Bottom of preview and chat: composer, transcript tail and the preview's Workaround section.
  chat: () => { const r = W($('content')), c = W(document.querySelector('.chat-col')), x = r.x + r.w / 2 - 10;
    return { x, y: c.y + c.h - WORK_H, w: c.x + c.w + EDGE - x, h: WORK_H }; },
  // Review's bar and columns, with the toolbar's Changes button above them.
  review: () => { const r = W($('content')); return { x: r.x - EDGE, y: W(dmx).y, w: r.w + EDGE + 10, h: 560 }; },
};
// [start, duration, shot, drift]: drift is a slow push while holding.
const CAMERA = [
  // The video cut holds the whole terminal under the title card, then pushes
  // in to the prompt shortly before /draftmux is typed.
  ...VIDEO ? [[-LEAD, 0, 'term'], [T.type1[0] - 1.3, .8, 'termTail', .02]] : [[0, 0, 'termTail', .02]],
  // One move from the terminal to the Draftmux window through the whole
  // desk, where the window launches.
  [6.9, 1.1, 'dmx', 0, 'desk'],
  [12.8, .8, 'source', .02],
  [16.8, .7, 'chat', .02],
  // The whole window while the rewrite lands, title to workaround.
  [20.6, .8, 'dmx'],
  [24.0, .8, 'source', .02],
  [32.3, .8, 'targets'],
  [38.0, .8, 'source', .02],
  [42.9, .6, 'steps'],
  // The comment is typed over the step; the camera takes in the chat while
  // it is typed, so the sent comment lands in view.
  [T.type4[0] + .3, .7, 'chat', .02],
  [51.2, .9, 'review'],
  // One move back to the terminal, out through the whole desk, the way the
  // window came in: the terminal comes forward as the camera passes the desk.
  [T.swap - .55, 1.1, 'termTail', .02, 'desk'],
];
// Shots other than the desk, the window and the terminal's tail fit above the
// caption band at the bottom.
const BAND = 150;
const DESK_MARGIN = 14;
function fit(r, shot) {
  if (shot === 'desk') return { cx: r.x + r.w / 2, cy: r.y + r.h / 2, s: Math.min(1920 / r.w, 1080 / r.h) };
  // The terminal's tail fills the frame's width with the terminal's bottom
  // edge on the frame's; the caption sits over the empty lines below the
  // prompt.
  if (shot === 'termTail') { const s = 1920 / r.w; return { cx: r.x + r.w / 2, cy: r.y + r.h - 540 / s, s }; }
  // The whole window, with a margin of the desk around it so it reads as a window.
  if (shot === 'dmx') { const s = Math.min(1920 / (r.w + 2 * DESK_MARGIN), 1080 / (r.h + 2 * DESK_MARGIN)); return { cx: r.x + r.w / 2, cy: r.y + r.h / 2, s }; }
  const s = Math.min(1920 / r.w, (1080 - BAND) / r.h);
  return { cx: r.x + r.w / 2, cy: r.y + r.h / 2 + BAND / 2 / s, s };
}
function cameraAt(t) {
  let i = 0; CAMERA.forEach((c, k) => { if (t >= c[0]) i = k; });
  const [start, dur, shot, drift = 0, via] = CAMERA[i];
  const to = fit(SHOTS[shot](), shot);
  let cam = to;
  window.__shot = shot;
  if (dur > 0 && t < start + dur && i > 0) {
    const prev = CAMERA[i - 1], from = fit(SHOTS[prev[2]](), prev[2]);
    from.s *= 1 + (prev[3] ?? 0);
    const p = smoother((t - start) / dur);
    window.__shot = `${prev[2]} → ${shot}`;
    cam = { cx: from.cx + (to.cx - from.cx) * p, cy: from.cy + (to.cy - from.cy) * p, s: Math.exp(Math.log(from.s) + (Math.log(to.s) - Math.log(from.s)) * p) };
    // A move `via` a shot passes through it halfway: the centre follows a
    // quadratic curve through the shot's centre, and the zoom dips to its scale.
    if (via) {
      const v = fit(SHOTS[via](), via), q = 1 - p;
      const ctl = { x: 2 * v.cx - (from.cx + to.cx) / 2, y: 2 * v.cy - (from.cy + to.cy) / 2 };
      cam.cx = q * q * from.cx + 2 * q * p * ctl.x + p * p * to.cx;
      cam.cy = q * q * from.cy + 2 * q * p * ctl.y + p * p * to.cy;
      const dip = Math.max(0, (Math.log(from.s) + Math.log(to.s)) / 2 - Math.log(v.s));
      cam.s *= Math.exp(-dip * Math.sin(Math.PI * p));
    }
  } else if (drift) {
    const next = CAMERA[i + 1]?.[0] ?? DURATION;
    cam = { ...to, s: to.s * (1 + drift * ease((t - start - dur) / (next - start - dur))) };
  }
  return cam;
}

// ---------- Cursor ----------
const pt = (r, fx = .5, fy = .5) => ({ x: r.x + r.w * fx, y: r.y + r.h * fy });
const CP = {
  center: () => pt(W($('content')), .45, .45),
  editorBtn: () => pt(W($('b-editor'))),
  // The end of the step the Markdown edit extends, and out of the way below
  // it, where the cursor waits while the edit is typed.
  mdStep: () => { const r = WR(textRange($('hl'), MD_AT - 1, MD_AT).getBoundingClientRect()); return { x: r.x + r.w + 2, y: r.y + r.h * .6 }; },
  belowStep: () => { const p = CP.mdStep(); return { x: p.x + 30, y: p.y + 70 }; },
  // The end of the description, where the continuation goes, and out of the
  // way below it while the sentence is typed and continued.
  contEnd: () => { const r = WR(textRange($('hl'), CONT_AT - 1, CONT_AT).getBoundingClientRect()); return { x: r.x + r.w + 2, y: r.y + r.h * .6 }; },
  belowCont: () => { const p = CP.contEnd(); return { x: p.x + 140, y: p.y + 16 }; },
  slackBtn: () => pt(W($('seg-slack'))),
  richBtn: () => pt(W($('seg-rich'))),
  githubBtn: () => pt(W($('seg-github'))),
  composer: () => pt(W($('cbox')), .35, .3),
  s2start: () => { const r = WR(previewRange(S2, 1).getBoundingClientRect()); return { x: r.x, y: r.y + r.h * .6 }; },
  selEnd: () => { const [text, n] = previewSelAt(window.__t); const rs = previewRange(text, Math.max(n, 1)).getClientRects(), r = WR(rs[rs.length - 1]); return { x: r.x + r.w, y: r.y + r.h * .6 }; },
  // Where the clicked popover item sat: the cursor rests there while
  // the popover runs, lists the options and closes.
  altItem: () => ALT_AT.findLast(([from]) => window.__t >= from)?.[1] ?? ALT_AT[0]?.[1] ?? { x: 0, y: 0 },
  stepStart: () => { const r = WR(previewRange(STEP, 1).getBoundingClientRect()); return { x: r.x, y: r.y + r.h * .6 }; },
  aboveS2: () => { const p = CP.s2start(); return { x: p.x + 60, y: p.y - 90 }; },
  reviewBtn: () => pt(W($('b-review'))),
  rangeBtn: () => pt(W($('rv-range')), .4),
  // Measured once the review and its menu are laid out (measureAlternatives).
  doneBtn: () => pt(W($('rv-done'))),
  sinceRow: () => pt(W($('rv-range'))),
};
// The popover item each pass clicks, measured in the frame before the click,
// from the start of that pass's drag.
const ALT_AT = [];
function measureAlternatives() {
  for (const [from, a, item] of [[T.drag3[0], T.alt3, 0], [T.drag4[0], T.comment4, COMMENT]]) {
    seek(LEAD + a - .02);
    ALT_AT.push([from, pt(W(document.querySelectorAll('#pop .pop-item')[item]), .5, .55)]);
  }
  // The Alternatives option the cursor moves to and clicks.
  CP.alt3opt = CP.altItem;
  seek(LEAD + T.opts3 + .3);
  // Straight down from the Alternatives item it clicked.
  const opt3 = { x: ALT_AT[0][1].x, y: pt(W(document.querySelectorAll('#pop .pop-opt')[1]), .5, .55).y };
  CP.alt3opt = () => opt3;
  // The range menu's Alternatives row, where the cursor rests once it closes.
  seek(LEAD + T.since - .05);
  const since = pt(W([...$('rvmenu').querySelectorAll('.menu-item')].find((e) => e.textContent.includes('Alternatives'))), .35);
  CP.sinceRow = () => since;
  // Done goes away with the review it closes; the cursor rests where it was.
  seek(LEAD + T.done - .05);
  const done = pt(W($('rv-done')));
  CP.doneBtn = () => done;
}
// [t, point]; the cursor eases between consecutive points within a group.
const CURSOR = [
  [[16.7, 'center'], [T.click2 - .15, 'composer'], [T.click2 + .3, 'composer']],
  [[23.8, 'center'], [T.toggle - .2, 'editorBtn'], [T.toggle + .3, 'editorBtn'], [T.mdClick - .05, 'mdStep'], [T.mdClick + .15, 'mdStep'], [T.typeMd[0] - .1, 'belowStep'], [T.typeMd[1] + .2, 'belowStep'],
    [T.contClick - .05, 'contEnd'], [T.contClick + .15, 'contEnd'], [T.typeCont[0] - .1, 'belowCont'], [T.accept + .3, 'belowCont'], [T.slack - .15, 'slackBtn'], [T.slack + .3, 'slackBtn'], [T.rich - .15, 'richBtn'], [T.rich + .3, 'richBtn'],
    [T.github - .15, 'githubBtn'], [T.github + .3, 'githubBtn']],
  [[38.3, 'aboveS2'], [T.drag3[0] - .05, 's2start'], [T.drag3[0], 's2start'], [T.drag3[1], 'selEnd'], [T.pop3 + .25, 'selEnd'], [T.alt3 - .1, 'altItem'], [T.alt3 + .4, 'altItem'], [T.hover3 - .35, 'altItem'], [T.hover3, 'alt3opt'], [T.apply3 + .1, 'alt3opt'],
    [T.drag4[0] - .1, 'stepStart'], [T.drag4[0], 'stepStart'], [T.drag4[1], 'selEnd'], [T.pop4 + .25, 'selEnd'], [T.comment4 - .12, 'altItem'], [T.comment4 + .3, 'altItem']],
  [[51.7, 'center'], [T.review - .15, 'reviewBtn'], [T.review + .3, 'reviewBtn'], [T.range - .15, 'rangeBtn'], [T.range + .3, 'rangeBtn'],
    [T.since - .45, 'sinceRow'], [T.since + .3, 'sinceRow'], [T.done - .5, 'doneBtn'], [T.done + .3, 'doneBtn']],
];
const CLICKS = [T.review, T.range, T.since, T.done, T.toggle, T.mdClick, T.contClick, T.slack, T.rich, T.github, T.click2, T.drag3[0], T.alt3, T.apply3, T.drag4[0], T.comment4];
function cursorAt(t) {
  const c = $('cursor');
  const g = CURSOR.find((g) => t >= g[0][0] - .3 && t < g[g.length - 1][0] + .3);
  c.style.display = g ? '' : 'none';
  if (!g) return;
  c.style.opacity = win(t, g[0][0] - .3, g[g.length - 1][0], .3);
  let pos;
  if (t <= g[0][0]) pos = CP[g[0][1]]();
  else if (t >= g[g.length - 1][0]) pos = CP[g[g.length - 1][1]]();
  else {
    const i = g.findIndex(([a]) => a > t), [a, ka] = g[i - 1], [b, kb] = g[i];
    const p0 = CP[ka](), p1 = CP[kb](), dx = p1.x - p0.x, dy = p1.y - p0.y, d = Math.hypot(dx, dy);
    if (/start$/.test(ka) && kb === 'selEnd') pos = p1;
    else {
      // A hand's move: a quick start and a long landing, along a slight arc
      // that bends alternately one way and the other.
      const x = clamp((t - a) / (b - a)), p = .35 * smoother(x) + .65 * (1 - Math.pow(1 - x, 4));
      const bend = d * .08 * Math.sin(Math.PI * p) * (i % 2 ? 1 : -1) / (d || 1);
      pos = { x: p0.x + dx * p - dy * bend, y: p0.y + dy * p + dx * bend };
    }
  }
  // A resting hand still drifts a little.
  const s0 = window.__cam.s;
  pos = { x: pos.x + (Math.sin(t * 1.3) + .6 * Math.sin(t * 3.1 + 1)) * .9 / s0, y: pos.y + (Math.sin(t * 1.7 + 2) + .5 * Math.sin(t * 2.6)) * .7 / s0 };
  const click = CLICKS.find((k) => Math.abs(t - k) < .1);
  const press = click != null ? 1 - .16 * (1 - Math.abs(t - click) / .1) : 1;
  // Constant on-screen size whatever the zoom.
  const s = window.__cam.s;
  c.style.transform = `translate(${pos.x - 3}px, ${pos.y - 2}px) scale(${press / s * 1.25})`;
}

// ---------- Screen-space overlays ----------
function captionAt(t) {
  const c = CAPTIONS.find(([a, b]) => t >= a && t < b);
  const pill = $('pill'), sub = $('sub');
  if (!c) { pill.style.opacity = 0; sub.style.opacity = 0; return; }
  const [a, b, text, tag, s] = c;
  setHtml(pill, `<span>${text}</span>${tag ? `<span class="tag">${modIcons(tag)}</span>` : ''}`);
  const k = win(t, a, b, .3);
  pill.style.opacity = k; pill.style.transform = `translateY(${(1 - easeOut((t - a) / .4)) * 14}px)`;
  setHtml(sub, s ? s[0] : '');
  sub.style.opacity = s ? win(t, s[1][0], s[1][1], .3) * k : 0;
}
function keysAt(t) {
  const k = KEYS.find(([a, b]) => t >= a && t < b);
  setHtml($('keys'), k ? k[2].map((x) => `<div class="key">${modIcons(x)}</div>`).join('') : '');
  if (!k) return;
  const p = win(t, k[0], k[1], .12), pop = easeOut((t - k[0]) / .18);
  $('keys').style.opacity = p; $('keys').style.transform = `translateY(${(1 - pop) * 10}px) scale(${.92 + .08 * pop})`;
}

// ---------- Windows ----------
function windowsAt(t) {
  // Draftmux: absent until /draftmux launches it.
  let a = 0, s = 1;
  // It grows out of the terminal that launched it.
  if (t >= T.launch) { const x = clamp((t - T.launch) / .6); a = clamp(x * 4); s = .3 + .7 * easeOutBack(x); }
  dmx.style.opacity = a; dmx.style.transform = `scale(${s})`;
  dmx.style.display = a > 0 ? '' : 'none';
  // Terminal comes forward for the read-back.
  const termFront = t >= T.swap;
  $('term').style.zIndex = termFront ? 2 : 0; dmx.style.zIndex = termFront ? 0 : 1;
  const behind = a > .5 && !termFront;
  $('tdim').style.opacity = behind ? .35 : 0;
  $('term').classList.toggle('inactive', behind);
  dmx.classList.toggle('inactive', termFront);
  const pop = termFront ? easeOut((t - T.swap) / .3) : 1;
  $('term').style.transform = termFront ? `scale(${.985 + .015 * pop})` : '';
  // It fades in over Draftmux. A copy of the terminal stays behind while it
  // does, so only the overlap cross-fades and the rest of the terminal stays put.
  const fadeIn = termFront ? ease((t - T.swap) / .35) : 1;
  $('term').style.opacity = fadeIn;
  // Off the desk while the camera is on Draftmux, from the end of the move in
  // to the start of the move back, so no shot shows it past the window's edge.
  const into = CAMERA.find((c) => c[2] === 'dmx' && c[4] === 'desk'), back = CAMERA[CAMERA.length - 1];
  $('term').style.visibility = t >= into[0] + into[1] && t < back[0] ? 'hidden' : '';
  $('termghost')?.remove();
  if (fadeIn < 1) {
    const ghost = $('term').cloneNode(true);
    ghost.querySelectorAll('[id]').forEach((e) => e.removeAttribute('id'));
    ghost.id = 'termghost';
    Object.assign(ghost.style, { zIndex: 0, opacity: 1, transform: '' });
    ghost.classList.add('inactive');
    ghost.querySelector('.dim').style.opacity = .35;
    $('term').before(ghost);
  }
}

// The first draft's tics, in reading order: [text around the tic, the tic].
// Each step marks its tics with a highlighter sweep, and the marks stay until
// the humanizer rewrite replaces the draft.
const TICS = [
  [["Here's the thing"]], [['load-bearing']], [['genuinely']], [["isn't just"], ['— it\'s']], [['structural']],
  [['Honestly']], [['quietly falls', 'quietly']], [['The result?']], [['landing squarely']],
];
// The sweeps overlap: each starts before the last one finishes.
const TIC_GAP = .15, TIC_SWEEP = .55;
function ticsAt(t) {
  const on = t >= T.tics && t < T.d2;
  const rects = !on ? [] : TICS.flatMap((step, i) => {
    const k = easeOut((t - T.tics - i * TIC_GAP) / TIC_SWEEP);
    if (k <= 0) return [];
    return step.flatMap(([around, tic = around]) => {
      const r = findRange($('pvbody'), around);
      if (!r) return [];
      r.setStart(r.startContainer, r.startOffset + around.indexOf(tic)); r.setEnd(r.startContainer, r.startOffset + tic.length);
      return [...r.getClientRects()].map((c) => { const b = pad(rel(c, $('pvscroll')), 3, 1); return { ...b, w: b.w * k }; });
    });
  });
  setHtml($('tics'), selHtml(rects));
}

// A joke on the first draft's "load-bearing": a beam of bricks on one thin
// stick pops up over the word, wobbles, and goes.
function loadBearingAt(t) {
  const img = $('lb'), [a, b] = T.lb;
  img.style.display = t >= a && t < b ? '' : 'none';
  if (t < a || t >= b) return;
  const word = [...$('pvbody').querySelectorAll('strong')].find((e) => e.textContent === 'load-bearing');
  const r = W(word), size = r.h * 8.5;
  const k = clamp((t - a) / .35);
  const wobble = Math.sin((t - a) * 18) * 3 * (1 - clamp((t - a) / .9));
  Object.assign(img.style, { width: `${size}px`, left: `${r.x + r.w / 2 - size / 2}px`, top: `${r.y - size - 4}px`,
    opacity: Math.min(clamp((t - a) / .12), 1 - clamp((t - (b - .25)) / .25)),
    transform: `rotate(${-4 + wobble}deg) scale(${.55 + .45 * easeOutBack(k)})`, transformOrigin: '50% 100%' });
}

// ---------- Frame ----------
// The title card holds, complete, from frame 0, then lifts off the session as
// the story starts.
// Other drafts Draftmux suits, shown on black before the end card.
// Each has an example draft, shown in its target's preview as its pill pops.
const USES = [
  ['README', 'github-file', `# git-surgeon

Gives AI agents surgical control over git changes without interactive prompts.

## Install

\`\`\`sh
brew install raine/git-surgeon/git-surgeon
\`\`\`

## Usage

\`\`\`sh
git-surgeon hunks
git-surgeon commit ac34353 15baf94 -m "add logging"
\`\`\``],
  ['Release notes', 'github', `## workmux v0.1.281

- Add experimental Zellij support for the agent sidebar (#331)
- Add GitHub Copilot CLI as a built-in agent (#314)
- Show Copilot agents as waiting when they ask for permission (#336)
- Fix Zellij commands occasionally failing when a pane closes`],
  ['PR descriptions', 'github', `## Summary

Add a \`--since\` filter, so search skips conversations older than a date and stays fast in long-lived projects.

## Changes

- Parse \`--since 2w\`, \`--since yesterday\` and ISO dates
- Skip session files by modification time before reading them
- Show the active filter in the search bar, with \`Esc\` to clear it

## Testing

- [x] Unit tests for relative and absolute dates
- [x] Searched a project with 2,000 sessions: 1.8s → 0.2s`],
  ['Design docs', 'github-file', `---
title: Sync conflicts in aven
---
## Problem

Two devices can edit the same task offline. The last write wins, and the other edit is lost without a trace.

## Proposal

- Keep both versions when a field conflicts, and flag the task
- Show the conflict in the TUI as a diff to pick from
- Resolve from the CLI with \`aven conflict resolve <ref>\`

## Alternatives

Field-level CRDTs would avoid most conflicts, but cost a rewrite of the sync layer.`],
  ['Landing pages', 'github-file', `# Run your agents side by side

workmux gives each coding agent its own git worktree and tmux window, so five tasks can run at once without stepping on each other.

- **One command per task:** \`workmux add fix-login\` creates the worktree, opens the window and starts the agent
- **See who needs you:** the sidebar shows which agents are working, waiting or done
- **Merge and move on:** \`workmux merge\` lands the branch and cleans up the worktree`],
  ['Slack updates', 'slack', `:white_check_mark: **Checkout latency is back to normal**

From 14:05 to 14:40 UTC, about 8% of checkout requests timed out after a config change halved the payments connection pool.

- **Fix:** we rolled the change back at 14:38, and p99 latency is under 300 ms again
- **Impact:** no orders were lost, and failed payments retried on their own
- **Next:** a CI check that blocks pool sizes below the baseline

Thanks @maya and @jon for the quick rollback. Write-up in #incidents tomorrow.`],
  ['Emails', 'rich', `---
subject: Pi sessions in claude-history
---
Hi Sam,

Thanks for asking about Pi. claude-history 0.1.83 searches Pi and Oh My Pi sessions next to Claude Code and Codex, so you can drop your export script.

If a Pi session doesn't show up, send me its project path and I'll take a look.

Best,
Raine`],
];
// The outro: black, then the other uses pop in one by one, each swapping in
// its example below, and leave; then the end card's icon and name rise in,
// then its two lines; then the card slides aside for the credits.
const USE_STEP = .5;
// Where each example lands on the pile: x and y offsets in px, tilt in degrees.
const PILE = [[-70, 6, -3], [60, -8, 2.5], [-30, 10, -1.5], [80, 0, 3], [-50, -4, -2.5], [40, 8, 2], [10, 4, 1]];
function outroAt(t) {
  const o = $('outro'), k = ease((t - T.outro) / .6);
  o.style.display = k > 0 ? '' : 'none';
  o.style.opacity = k;
  const rise = (e, at) => { e.style.opacity = fade(t, at, .5); e.style.transform = `translateY(${(1 - easeOut((t - at) / .7)) * 18}px)`; };
  const uses = o.querySelector('.uses'), list = uses.querySelector('ul'), [a, b] = T.uses;
  const ex = uses.querySelector('.ex');
  if (!list.children.length) {
    list.innerHTML = USES.map(([u]) => `<li>${u}</li>`).join('');
    ex.innerHTML = USES.map(([, target, md]) => `<div><div class="body">${MD.renderPreview(md, target).html}</div></div>`).join('');
  }
  uses.style.display = t < b + .4 ? '' : 'none';
  uses.style.opacity = 1 - fade(t, b, .4);
  rise(uses.firstElementChild, a);
  const at = (i) => a + .6 + i * USE_STEP;
  [...list.children].forEach((li, i) => {
    li.style.opacity = fade(t, at(i), .25);
    li.style.transform = `scale(${.8 + .2 * easeOutBack(clamp((t - at(i)) / .4))})`;
    li.classList.toggle('on', t >= at(i) && (i === USES.length - 1 || t < at(i + 1)));
  });
  // Each example drops onto the pile at its own tilt and offset, landing
  // with a slight overshoot; the card under it gives a little on impact, and
  // older ones dim.
  [...ex.children].forEach((card, i) => {
    const p = clamp((t - at(i)) / .4), inK = easeOut(p), land = easeOutBack(p, 1.6), [dx, dy, rot] = PILE[i];
    let d = 0;
    for (let j = i + 1; j < USES.length; j++) d += ease(clamp((t - at(j)) / .3));
    const hit = i + 1 < USES.length ? clamp((t - at(i + 1) - .1) / .3) : 0, bump = Math.sin(hit * Math.PI) * (1 - hit);
    card.style.opacity = clamp((t - at(i)) / .1);
    card.style.transform = `translate(${dx}px, ${dy - (1 - inK) * 40 + bump * 6}px) rotate(${rot * (2 - inK)}deg) scale(${1.12 - .12 * land})`;
    card.style.filter = d > 0 ? `brightness(${1 - Math.min(d, 3) * .06})` : '';
    card.style.visibility = t >= at(i) ? '' : 'hidden';
  });
  const [img, name, l2, l3] = o.querySelector('.card').children, c = T.card;
  rise(img, c); rise(name, c + .2); rise(l2, c + .9); rise(l3, c + 1.3);
  img.style.transform += ` scale(${.9 + .1 * easeOut((t - c) / .7)})`;
  creditsAt(t);
}
// The end card slides left, as TV credits do, a rule draws down beside it,
// and the credits roll up past it at a constant speed; then everything
// fades to black.
function creditsAt(t) {
  const o = $('outro'), roll = o.querySelector('.roll'), rule = o.querySelector('.rule');
  if (!roll.children.length) {
    roll.innerHTML = CREDITS.map(([role, v, note]) => `<div><div class="role">${role}</div><div class="who">${v ?? `${Math.round(LEAD + DURATION)} seconds`}${note ? `<small>${note}</small>` : ''}</div></div>`).join('');
  }
  const k = ease((t - T.squeeze) / .9), card = o.querySelector('.card'), dark = 1 - fade(t, T.dark, 1.2);
  card.style.transform = `translateX(${-440 * k}px) scale(${1 - .14 * k})`;
  card.style.opacity = dark;
  rule.style.opacity = fade(t, T.squeeze + .6, .4) * dark;
  rule.style.transform = `scaleY(${easeOut((t - T.squeeze - .6) / .8)})`;
  roll.parentElement.style.display = t >= T.roll ? '' : 'none';
  roll.style.transform = `translateY(${1080 - (t - T.roll) * ROLL_SPEED}px)`;
}
// The video cut's title card holds, complete, from frame 0, then lifts off
// the session as the story starts.
function titleAt(t) {
  const out = ease((t - TITLE[0]) / (TITLE[1] - TITLE[0]));
  $('title').style.opacity = 1 - out;
  $('title').style.display = out >= 1 ? 'none' : '';
  world.style.filter = out < 1 ? `blur(${(8 * (1 - out)).toFixed(2)}px)` : '';
}
// seek takes scene time; everything below it works in story time.
function seek(sceneT) {
  const t = sceneT - LEAD;
  window.__t = t;
  const ct = t, termT = t;
  windowsAt(t);
  draftmuxAt(ct);
  terminalAt(termT);
  window.__t = ct;
  const cam = HERO ? heroCam() : cameraAt(t);
  window.__cam = cam;
  // The video cut's wordmark shows over the opening terminal shot until the
  // push to the prompt, and on the desk.
  if (VIDEO) $('brand').style.opacity = Math.max(1 - clamp((cam.s - 1.05) / .15), 1 - clamp((t - CAMERA[1][0]) / .4)) * (1 - ease((t - T.outro) / .6));
  world.style.transform = `translate(${960 - cam.cx * cam.s}px, ${540 - cam.cy * cam.s}px) scale(${cam.s})`;
  cursorAt(t);
  captionAt(t);
  loadBearingAt(t);
  outroAt(t);
  keysAt(t);
  if (VIDEO) titleAt(t);
  if (HERO) for (const id of ['cursor', 'cap', 'keys', 'title', 'brand', 'lb']) $(id).style.display = 'none';
}
// video.html#hero draws the title card's screenshot (img/hero.webp): the
// Alternatives popover's options with the whole window filling the frame, no
// overlays. history/2026-11-10-demo-video/hero-shot.cjs captures it at 4x.
const HERO = VIDEO && location.hash === '#hero', HERO_T = 41.9;
function heroCam() {
  const r = W(dmx), s = Math.min(1920 / r.w, 1080 / r.h);
  return { cx: r.x + r.w / 2, cy: r.y + r.h / 2, s };
}
// Extra lines for the player's annotations; the player adds the element
// under the region itself.
window.inspect = (rect) => {
  const { cx, cy, s } = window.__cam;
  if (!rect) return [`Camera: shot "${window.__shot}" at zoom ${s.toFixed(2)}`];
  const { x, y, w, h } = rect, wx = (x - 960) / s + cx, wy = (y - 540) / s + cy;
  return [`Camera: shot "${window.__shot}" at zoom ${s.toFixed(2)}; the region in world coordinates is x=${Math.round(wx)} y=${Math.round(wy)} w=${Math.round(w / s)} h=${Math.round(h / s)}`];
};

window.DURATION = LEAD + DURATION;
window.SEGMENTS = [
  ...VIDEO ? [{ id: 'title', label: 'Title', start: -LEAD, end: 0 }] : [],
  ...SEGMENTS,
].map((g) => ({ ...g, start: g.start + LEAD, end: g.end + LEAD }));
window.seek = seek;
window.FPS = 60;
// Only the video cut has the title card, so only it fetches the card's screenshot.
const heroLoaded = VIDEO ? Object.assign($('title').querySelector('.hero'), { src: 'img/hero.webp' }).decode().catch(() => {}) : null;
window.ready = Promise.all([heroLoaded, document.fonts.ready.then(() => Promise.all([...document.fonts].map((f) => f.load())))])
  .then(() => { measureAlternatives(); seek(HERO ? LEAD + HERO_T : 0); });
