import assert from "node:assert/strict";
import test from "node:test";
import { applyApprovalText, applyRequest, presentApplyPreview } from "../presentation.mjs";
import { registerDsh } from "../dsh.mjs";
import registerPi from "../pi.mjs";

const input = { selector: "/state/task/stage", paths: ["src/main.py"] };
const base = {
  schema_version: 1, operation: "apply", mode: "preview", status: "ready",
  source: { stage: input.selector, session_id: "session", execution_id: "execution", generation: 0 },
  scope: { kind: "project", target: "/project" },
  selected: [{ path: "src/main.py", kind: "modified", old_type: "file", new_type: "file", size_bytes: 14 }],
  required: [], blockers: [], candidates: [{ path: "scratch.txt", kind: "added" }],
  remaining: { in_scope: 1, outside_scope: 0 }, execution: null, plan: "native-plan-token",
};
const diff = "--- src/main.py\n+++ src/main.py\n@@ -1 +1 @@\n-old\n+new\n";
function preview(plan = base, options = {}) {
  return presentApplyPreview({
    project: "/project", selector: input.selector, exit_code: 0, interrupted: false, signal: null,
    truncated: false, stderr: "", stdout: JSON.stringify(plan), ...options,
  }, input, diff);
}

test("readable apply summary separates requested/selected/dependencies/remaining and selected diff", () => {
  const shown = preview();
  assert.equal(shown.approval_ready, true);
  assert.equal(shown.preview_state, "ready");
  assert.match(shown.summary, /Project: "\/project"/);
  assert.match(shown.summary, /MODIFY "src\/main.py"; new size 14 B/);
  assert.match(shown.summary, /Remaining candidates: in scope 1, outside scope 0/);
  assert.match(shown.summary, /\+new/);
  assert.ok(!shown.summary.includes("scratch.txt"), "Unselected inventory must not appear as approved changes");
  assert.ok(!shown.summary.includes("schema_version"));
  assert.ok(!shown.summary.includes("native-plan-token"));
  assert.deepEqual(JSON.parse(shown.stdout), base, "Keep machine-readable JSON separate from the native diff");
  assert.equal(shown.diff, diff);
});

test("blocked preview explains ancestor conflicts and does not present candidates as admitted changes", () => {
  const plan = { ...base, status: "blocked", selected: [], candidates: [
    { path: "experiments/pvisor-case/scratch.txt", kind: "modified", size_bytes: 14 },
  ], blockers: ["target changed after staging at /project/experiments/pvisor-case; refusing to overwrite concurrent changes"] };
  const shown = preview(plan, { exit_code: 3 });
  assert.equal(shown.approval_ready, false);
  assert.match(shown.summary, /this request wrote no project files/);
  assert.match(shown.summary, /Selected native changes: 0/);
  assert.match(shown.summary, /Candidate inventory \(not admitted for apply\): 1/);
  assert.match(shown.summary, /Blocked because:[\s\S]*target changed after staging/);
  assert.match(shown.summary, /Apply will not be requested/);
  assert.ok(!shown.summary.includes("+new"), "Do not show a stale selected diff for a blocked plan");
});

test("malformed, foreign, interrupted or truncated previews cannot authorize apply", () => {
  const cases = [
    preview(base, { stdout: "not json" }),
    preview(base, { interrupted: true }),
    preview(base, { truncated: true }),
    preview({ ...base, schema_version: 2 }),
    preview({ ...base, scope: { kind: "rootfs", target: "/" } }),
    preview({ ...base, source: { ...base.source, stage: "/foreign/stage" } }),
  ];
  for (const shown of cases) {
    assert.equal(shown.approval_ready, false);
    assert.match(shown.summary, /UNAVAILABLE/);
  }
});

test("summary bounds long inventories and diffs without hiding that display is incomplete", () => {
  const selected = Array.from({ length: 100 }, (_, i) => ({ path: `file-${i}`, kind: "added" }));
  const shown = presentApplyPreview({
    project: "/project", exit_code: 0, stdout: JSON.stringify({ ...base, selected }),
  }, input, "+line\n".repeat(10_000));
  assert.match(shown.summary, /80 more changes/);
  assert.match(shown.summary, /\[truncated\]/);
  assert.ok(Buffer.byteLength(shown.summary) < 25 * 1024);
  assert.equal(shown.approval_ready, true, "Display excerpting is explicit; full native plan remains available");
});

test("empty plans are informational no-ops, not approval requests", () => {
  const shown = preview({ ...base, selected: [], candidates: [] });
  assert.equal(shown.approval_ready, false);
  assert.equal(shown.preview_state, "no_changes");
  assert.match(shown.summary, /nothing to apply/);
});

test("filenames, blockers and agent text cannot inject terminal/bidi controls into the display", () => {
  const shown = preview({ ...base, status: "blocked", selected: [],
    candidates: [{ path: "src/\u202eodd\nfile", kind: "modified" }],
    blockers: ["conflict \u001b[31mred\u202e"],
  }, { exit_code: 3 });
  const text = applyApprovalText(shown, "intent \u001b[31mtext\u202e");
  assert.ok(!text.includes("\u001b"));
  assert.ok(!text.includes("\u202e"));
  assert.match(text, /\\u001b/);
  assert.match(text, /odd\\nfile/);
  assert.equal(shown.approval_ready, false);
});

test("agent explanation is labeled unverified and never forwarded as native selection or authority", () => {
  const parsed = applyRequest({ ...input, summary: "Replace the slow lookup; tests still need verification." });
  assert.deepEqual(parsed.request, input);
  const text = applyApprovalText(preview(), parsed.explanation);
  assert.match(text, /Agent summary \(unverified\)/);
  assert.match(text, /Replace the slow lookup/);
  assert.ok(!Object.hasOwn(parsed.request, "summary"));
  assert.throws(() => applyRequest({ ...input, summary: "x".repeat(513) }), /512/);
  assert.throws(() => applyRequest(input, { requireSummary: true }), /explain/);
  assert.throws(() => applyRequest({ ...input, summary: " " }, { requireSummary: true }), /explain/);
});

for (const kind of ["dsh", "pi"]) {
  test(`${kind} confirmation is compact even when the host collapses whitespace; details stay in preview`, async () => {
    const shown = preview();
    const calls = [];
    let message;
    const client = {
      operate: async (action, request) => {
        calls.push({ action, request });
        return action === "preview" ? shown : { exit_code: 0 };
      },
    };
    const args = { ...input, summary: "Update the selected implementation." };
    if (kind === "dsh") {
      const definitions = new Map();
      registerDsh({
        tools: { register: (tool) => definitions.set(tool.name, tool) },
        approval: { request: async (request) => { message = request.reason; return "allowed-once"; } },
      }, (tool) => tool, () => client);
      await definitions.get("pvisor_apply").execute(args, {
        agent: { session: { header: { cwd: "/project" } } }, signal: new AbortController().signal,
      });
    } else {
      const commands = new Map();
      registerPi({ registerTool() {}, registerCommand: (name, command) => commands.set(name, command), on() {} },
        { createBackend: () => client });
      await commands.get("pvisor").handler(`apply ${JSON.stringify(args)}`, {
        cwd: "/project", hasUI: true,
        ui: { confirm: async (_title, text) => { message = text; return true; }, notify() {} },
      });
    }
    assert.match(message, /MODIFY "src\/main.py"/);
    assert.ok(!message.includes("+new"), "Raw diff must not be placed in the approval card");
    assert.ok(!message.includes("Native selected-change preview"));
    assert.ok(!message.includes("/state/task/stage"), "Do not repeat the full selector in the card");
    assert.match(shown.summary, /\+new/, "Full details remain in the preview result");
    assert.ok(Buffer.byteLength(message.replace(/\s+/g, " ")) < 1200);
    assert.match(message, /Update the selected implementation/);
    assert.ok(!message.includes("schema_version"));
    assert.deepEqual(calls, [{ action: "preview", request: input }, { action: "apply", request: input }]);
  });
}

test("approval card bounds long intent and multi-file scope without pretending to show every path", () => {
  const selected = Array.from({ length: 40 }, (_, i) => ({ path: `directory/${"x".repeat(300)}/file-${i}`, kind: "added" }));
  const shown = preview({ ...base, selected, remaining: { in_scope: 3, outside_scope: 0 } });
  const text = applyApprovalText(shown, "intent ".repeat(70));
  assert.match(text, /first 3 shown, 37 more in preview/);
  assert.match(text, /Remaining staged: 3/);
  assert.ok(Buffer.byteLength(text) < 1600);
  assert.ok(!text.includes("Native selected-change preview"));
});

test("compact approval preserves distinct path identities when HTML collapses spaces", () => {
  const selected = [
    { path: "src/a  b.py", kind: "modified" },
    { path: "src/a b.py", kind: "modified" },
  ];
  const text = applyApprovalText(preview({ ...base, selected }), "Update two distinct files.")
    .replace(/\s+/g, " ");
  assert.match(text, /a\\u0020\\u0020b\.py/);
  assert.match(text, /"src\/a b\.py"/);
});
