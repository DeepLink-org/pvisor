import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import test from "node:test";
import { monitorHandler, registerMonitor, MONITOR_ROUTE } from "../dsh-monitor.mjs";

test("DSH bundle mounts the package root so the real host discovers its browser half", async () => {
  const pkg = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
  const patch = await readFile(new URL("../cordis.patch.yml", import.meta.url), "utf8");
  assert.ok(patch.split("\n").some((line) => line.trim() === `name: ${pkg.name}`));
  assert.ok(!patch.includes(`name: ${pkg.name}/dsh`), "DSH 0.2.2-rc.1 skips named subpaths during client discovery");
  assert.equal(pkg.exports["."], "./dsh.mjs");
  assert.equal(pkg.exports["./client"], "./dsh-client.js");
  const root = await import("pvisor-agent-plugins");
  const alias = await import("pvisor-agent-plugins/dsh");
  assert.equal(root.apply, alias.apply);
  assert.equal(typeof root.apply, "function");
});

async function client(react = {}, primitives = {}) {
  let entry;
  vm.runInNewContext(await readFile(new URL("../dsh-client.js", import.meta.url), "utf8"), {
    window: { fetch: async () => assert.fail("Rendering must not issue requests"),
      __ModuleLoader__: { load: (value) => { entry = value; } } },
    document: { visibilityState: "visible" },
    AbortController, URLSearchParams, TextEncoder, setTimeout, clearTimeout,
  });
  assert.equal(entry.id, "pvisor-agent-plugins");
  return entry.factory((id) => {
    if (id === "@deepseek-ai/dsh-client-ui-primitives") return primitives;
    assert.equal(id, "react");
    return react;
  });
}

function fixture() {
  const calls = [];
  const backend = {
    project: async () => "/project",
    tasks: async (input) => {
      calls.push(["tasks", input]);
      return { tasks: [{ task: "one", selector: "/state/one/stage", state: "failed", filesystem: "staged" }],
        next_cursor: "next-page" };
    },
    operate: async (action, input) => {
      calls.push([action, input]);
      if (input.selector !== "/state/one/stage") throw new Error("Selector does not belong to project");
      if (action === "status") return { exit_code: 0, stdout: JSON.stringify({
        schema_version: 3, state: "failed", live: false,
        run: { stage: input.selector, overlay: { target: "/project", state: "staged", generation: 2 } },
      }) };
      if (action === "review") return { review_status: "complete",
        changes: [{ path: "result", kind: "added", new_type: "file" }],
        stdout: input.include_diff ? "Project diff (environment changes: use review --json)\n--- a/result\n+++ b/result\n@@ -0,0 +1 @@\n+candidate\n" : "",
        diff_status: input.include_diff ? "separate_observation" : "not_collected",
        counts: { total: 1, file: 1 }, in_scope: 1, outside_scope: 0, unclassified: 0 };
      throw new Error(`Mutation forbidden: ${action}`);
    },
    output: async () => ({ output_status: "complete", stdout: "task output", stderr: "",
      task_state: "failed", task_exit_code: 7, truncated: true }),
  };
  const ctx = { sessions: { get: (id) => id === "session" ? { header: { cwd: "/project" } } : undefined },
    get: () => undefined };
  backend.lifecycle = (input, signal) => backend.operate("status", input, signal);
  const handler = monitorHandler(ctx, (project) => { assert.equal(project, "/project"); return backend; });
  const request = (params, options) => handler(new Request(`http://local${MONITOR_ROUTE}?${params}`, options));
  return { ctx, backend, calls, handler, request };
}

test("terminal uses a new DSH Bash tab and sends one literal same-instance exec, never replaying", async () => {
  const plugin = await client();
  const writes = [], closed = [];
  let tabs = [];
  let subscriptions = 0;
  const observable = (value) => ({
    getSnapshot: value,
    subscribe: (callback) => { subscriptions++; callback(); return () => subscriptions--; },
  });
  const view = { id: "terminal-one",
    state: observable(() => ({ phase: "connected", writable: true })),
    write: (command) => writes.push(command), rename: async () => {} };
  const terminals = {
    launchShells: async () => ({ shells: [{ path: "/bin/bash" }] }),
    view: () => view,
    close: (...args) => closed.push(args),
  };
  const sidebar = {
    mounted: observable(() => "session"),
    openTabs: observable(() => tabs),
    openTab: (kind, options) => {
      assert.equal(kind, "terminal"); assert.equal(options.params.shellPath, "/bin/bash");
      tabs = [{ sessionId: "session", tabId: "new", contentId: "content", kind }];
    },
    close: () => assert.fail("Successful launch must keep the terminal"),
  };
  const ctx = { get: (key) => ({ webTerminals: terminals, sidebarRight: sidebar })[key] };
  const selector = "/state/it's a stage";
  const fetcher = async () => Response.json({
    schema_version: 1, selector, terminal_status: "ready", session_id: "native",
    argv: ["/usr/bin/pvisor", "exec", "--session", "-it", selector, "/bin/sh"],
  });
  const result = await plugin.openDshTerminal(ctx, "session", selector, undefined, fetcher);
  assert.equal(result.status, "launch_sent");
  assert.equal(writes.length, 1);
  assert.match(writes[0], /^builtin exec -- /);
  assert.ok(writes[0].includes("'/state/it'\\''s a stage'"));
  assert.ok(writes[0].endsWith("; builtin exit\r"));
  assert.equal(closed.length, 0);
  assert.equal(subscriptions, 0);
  await assert.rejects(plugin.openDshTerminal(ctx, "session", selector, undefined,
    async () => Response.json({ error: "Session is stopped" }, { status: 409 })), /stopped/);
  assert.equal(writes.length, 1);
});

test("monitor binds project to Session and only reads paginated tasks, selected changes and output", async () => {
  const f = fixture();
  const page = await f.request("session=session&cursor=first");
  assert.equal(page.headers.get("cache-control"), "no-store");
  assert.equal((await page.json()).tasks.length, 1);
  assert.deepEqual(f.calls[0], ["tasks", { cursor: "first", limit: 10, order: "newest" }]);
  const detail = await (await f.request("session=session&selector=/state/one/stage")).json();
  assert.equal(detail.detail_status, "complete");
  assert.equal(detail.counts.file, 1);
  assert.deepEqual(f.calls.at(-1), ["review", { selector: "/state/one/stage", include_diff: true }]);
  assert.equal(detail.files[0].additions, 1);
  assert.equal(detail.files[0].deletions, 0);
  const diff = await (await f.request("session=session&selector=/state/one/stage&view=diff")).json();
  assert.match(diff.diff, /\+candidate/);
  assert.equal(diff.diff_status, "separate_observation");
  assert.deepEqual(f.calls.at(-1), ["review", { selector: "/state/one/stage", include_diff: true }]);
  const output = await (await f.request("session=session&selector=/state/one/stage&view=output")).json();
  assert.equal(output.task_exit_code, 7);
  assert.equal(output.output_truncated, true);
  assert.ok(f.calls.every(([action]) => ["tasks", "status", "review"].includes(action)));
});

test("monitor refuses unknown sessions, foreign selectors, overrides, duplicate params and mutation requests", async () => {
  const f = fixture();
  for (const params of [
    "session=session&project=/other", "session=session&binary=/bin/sh", "session=session&view=apply",
    "session=session&session=other", "session=session&selector=x&cursor=y", "session=",
  ]) assert.equal((await f.request(params)).status, 400);
  assert.equal((await f.request("session=session", { method: "POST" })).status, 405);
  assert.equal((await f.request("session=missing")).status, 404);
  assert.equal(f.calls.length, 0);
  assert.equal((await f.request("session=session&selector=/foreign/stage")).status, 422);
});

test("running and unknown tasks do not masquerade as empty candidates or captured live logs", async () => {
  for (const state of ["running", "unknown"]) {
    const f = fixture();
    f.backend.operate = async (action, input) => {
      assert.equal(action, "status");
      return { exit_code: 0, stdout: JSON.stringify({
        schema_version: 3, state, live: state === "running",
        run: { stage: input.selector, overlay: { target: "/project", state: "staged" } },
      }) };
    };
    f.backend.output = () => assert.fail("must not request live output");
    const result = await (await f.request("session=session&selector=/state/one/stage")).json();
    assert.equal(result.detail_status, "not_available");
    assert.equal(result.changes, undefined);
  }
});

test("native identity/truncation failures are explicit and stored Sessions resolve through persistence", async () => {
  const f = fixture();
  f.ctx.sessions.get = () => undefined;
  f.ctx.get = () => ({ stat: async () => ({ header: { cwd: "/project" } }) });
  assert.equal((await f.request("session=stored")).status, 200);
  for (const observed of [
    { exit_code: 1 }, { exit_code: 0, truncated: true },
    { exit_code: 0, stdout: "{}" }, { exit_code: 0, stdout: "invalid" },
  ]) {
    f.backend.operate = async () => observed;
    assert.equal((await f.request("session=stored&selector=/state/one/stage")).status, 422);
  }
});

test("monitor caps concurrent reads and propagates request cancellation without retry", async () => {
  const f = fixture();
  const waits = [];
  f.backend.tasks = (_input, signal) => new Promise((resolve, reject) => {
    waits.push(() => resolve({ tasks: [], next_cursor: null }));
    signal.addEventListener("abort", () => reject(signal.reason), { once: true });
  });
  const controller = new AbortController();
  const first = f.request("session=session", { signal: controller.signal });
  const pending = Array.from({ length: 3 }, () => f.request("session=session"));
  await tick();
  assert.equal((await f.request("session=session")).status, 429);
  controller.abort();
  assert.equal((await first).status, 408);
  for (const release of waits) release();
  for (const result of await Promise.all(pending)) assert.equal(result.status, 200);
});

test("monitor registration uses only DSH's authenticated exact Fetch route and owns teardown", () => {
  const routes = [];
  let disposed = false;
  const web = { connection: { fetch: { register: (route) => { routes.push(route); return () => { disposed = true; }; } } },
    effect: (fn) => fn(), sessions: {} };
  const dispose = registerMonitor({ inject: (services, fn) => {
    assert.deepEqual(services, ["connection", "sessions"]);
    return fn(web);
  } });
  assert.equal(routes[0].path, MONITOR_ROUTE);
  assert.deepEqual(routes[0].methods, ["GET"]);
  // Fixture effect executes immediately; the production Cordis scope retains the disposer.
  routes.length = 0;
  web.effect = (fn) => { fn()(); };
  registerMonitor({ inject: (_services, fn) => fn(web) });
  assert.equal(disposed, true);
  assert.equal(dispose, undefined);
});

test("browser registers the public right Sidebar tab and its Session-scoped body", async () => {
  const entry = await client();
  const registrations = [];
  entry.apply({
    effect: (fn) => fn(),
    sidebarRightTabs: { register: (definition) => { registrations.push(definition); } },
    slots: { inject: (name, fn) => { assert.equal(name, "sidebar.right.pane.tab"); fn(); },
      register: (definition, body) => { registrations.push(definition); assert.equal(typeof body, "function"); } },
  });
  assert.equal(registrations[0].kind, "pvisor-monitor");
  assert.equal(registrations[1].key, registrations[0].id);
});

test("empty monitor presents actions, not persistent workflow explanations", async () => {
  const state = { sessionId: "session", tasks: [{ task: "one", selector: "/stage",
    state: "completed", filesystem: "discarded" }], previous: [],
    page: { project: "/home/user/Pulsing", next_cursor: "next" } };
  const states = [state, true, false];
  const entry = await client({
    createElement: (type, props, ...children) => ({ type, props, children }),
    Fragment: "fragment", useState: () => [states.shift(), () => {}],
    useMemo: (fn) => fn(), useEffect() {},
  }, { Button: "button", Pill: "button", Tag: "span" });
  let Body;
  entry.apply({
    effect: (fn) => fn(), sidebarRightTabs: { register() {} },
    slots: { inject: (_name, fn) => fn(), register: (_definition, body) => { Body = body; } },
  });
  const tree = Body({ sessionId: "session", useTabInfo: () => ({
    tab: { visible: true, signal: new AbortController().signal, actions: {} },
  }) });
  function visibleText(node) {
    if (node == null || typeof node === "boolean") return "";
    if (typeof node !== "object") return String(node);
    if (Array.isArray(node)) return node.map(visibleText).join(" ");
    const children = node.type === "details" ? node.children.filter((child) => child?.type === "summary") : node.children;
    return visibleText(children);
  }
  const text = visibleText(tree);
  assert.match(text, /本页暂无待处理任务/);
  assert.match(text, /查看全部/);
  assert.match(text, /下一页/);
  assert.match(text, /Pulsing/);
  for (const removed of ["不运行任务", "每页 10 项", "已隐藏", "保留任务元数据", "/home/user", "上一页"]) {
    assert.ok(!text.includes(removed), `Explanatory or unavailable text must not be persistent: ${removed}`);
  }
});

test("task presentation uses human titles, one main state and relative Chinese time", async () => {
  const { presentation: p } = await client();
  const task = { command: ["sh", "-c", "sh experiments/pvisor-lab/work.sh longbuild"],
    state: "running", live: true, filesystem: "active", started_at_unix_ms: 1000 };
  assert.equal(p.taskTitle(task), "longbuild");
  assert.equal(p.commandText(task), "sh -c sh experiments/pvisor-lab/work.sh longbuild");
  assert.equal(p.taskTitle({ command: ["python3", "scripts/verify_invoice.py"] }), "verify_invoice");
  assert.equal(p.taskTitle({ name: "修复折扣", command: ["sh"] }), "修复折扣");
  assert.equal(p.mainStatus(task)[0], "运行中");
  assert.equal(p.mainStatus({ state: "failed", filesystem: "staged" })[0], "失败");
  assert.equal(p.mainStatus({ state: "completed", filesystem: "staged", changed_files: 2 })[0], "待审阅");
  assert.equal(p.mainStatus({ state: "completed", filesystem: "discarded" })[0], "已清理");
  assert.equal(p.duration(task, 135000), "2分14秒");
  assert.equal(p.duration({ state: "completed", started_at_unix_ms: 1000 }, 135000), "");
  assert.equal(p.relativeTime(1000, 135000), "2分钟前");
  assert.equal(p.relativeTime(null, 135000), "时间未知");
});

const tick = () => new Promise((resolve) => setImmediate(resolve));

test("visible monitor polls sequentially, paginates, selects exact tasks and aborts on hide", async () => {
  const { createMonitor } = await client();
  const calls = [];
  const timers = new Map();
  let value;
  const model = createMonitor({
    sessionId: "session", publish: (state) => { value = state; },
    schedule: (fn) => { const id = timers.size + 1; timers.set(id, fn); return id; },
    cancel: (id) => timers.delete(id),
    fetch: async (url, options) => {
      calls.push({ url, options });
      const params = new URL(url, "http://host/prefix/").searchParams;
      return Response.json({ schema_version: 1, observed_at: "now", project: "/project",
        ...(params.has("selector") ? { selector: params.get("selector"), detail_status: "complete", changes: [] } :
          { tasks: [{ selector: "/state/one/stage" }], next_cursor: params.has("cursor") ? null : "next" }) });
    },
  });
  assert.equal(calls.length, 0);
  model.visible(true);
  await tick();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].options.credentials, "same-origin");
  assert.ok(calls[0].url.startsWith("api/pvisor/monitor?"));
  model.select("/foreign/stage");
  assert.equal(calls.length, 1);
  model.select("/state/one/stage");
  await tick();
  assert.equal(value.detail.detail_status, "complete");
  assert.match(calls.at(-1).url, /view=changes/);
  model.view("output");
  await tick();
  assert.match(calls.at(-1).url, /view=output/);
  model.next();
  await tick();
  assert.equal(value.cursor, "next");
  assert.equal(value.selected, null);
  model.previous();
  await tick();
  assert.equal(value.cursor, null);
  model.visible(false);
  assert.equal(timers.size, 0);
  assert.equal(calls.at(-1).options.signal.aborted, true);
  model.dispose();
});

test("late responses after hide/disposal cannot overwrite a newer observation; errors retain stale data", async () => {
  const { createMonitor } = await client();
  let finish;
  let value;
  let fail = false;
  const timers = new Map();
  const model = createMonitor({
    sessionId: "one", publish: (state) => { value = state; },
    schedule: (fn) => { timers.set(1, fn); return 1; }, cancel: (id) => timers.delete(id),
    fetch: () => fail ? Promise.reject(new Error("offline")) : new Promise((resolve) => { finish = resolve; }),
  });
  model.visible(true);
  model.visible(false);
  finish(Response.json({ schema_version: 1, observed_at: "old", project: "/old", tasks: [] }));
  await tick();
  assert.equal(value.page, null);
  fail = true;
  model.visible(true);
  await tick();
  assert.equal(value.error, "offline");
  assert.equal(value.page, null);
  model.dispose();
  assert.equal(timers.size, 0);
});

test("failed refresh preserves a timestamped prior page instead of inventing an empty inventory", async () => {
  const { createMonitor } = await client();
  let value;
  let offline = false;
  const model = createMonitor({
    sessionId: "one", publish: (state) => { value = state; },
    schedule: () => 1, cancel() {},
    fetch: async () => {
      if (offline) throw new Error("offline");
      return Response.json({ schema_version: 1, observed_at: "before failure", project: "/project",
        tasks: [{ selector: "/stage", state: "unknown" }], next_cursor: null });
    },
  });
  model.visible(true);
  await tick();
  offline = true;
  await model.refresh();
  assert.equal(value.page.observed_at, "before failure");
  assert.equal(value.tasks[0].state, "unknown");
  assert.equal(value.error, "offline");
  model.dispose();
});

test("background sampling is silent and publishes only changed task data", async () => {
  const { createMonitor } = await client();
  const publications = [];
  const timers = new Map();
  let clock = 0;
  let tasks = [{ selector: "/stage", state: "running", live: true, filesystem: "active" }];
  const model = createMonitor({
    sessionId: "one", publish: (state) => publications.push(state),
    schedule: (fn, ms) => { timers.set(1, { fn, ms }); return 1; },
    cancel: (id) => timers.delete(id),
    fetch: async () => Response.json({ schema_version: 1, observed_at: String(clock++),
      project: "/project", tasks, next_cursor: null }),
  });
  model.visible(true);
  await tick();
  const previous = publications.at(-1);
  const count = publications.length;
  assert.equal(previous.busy, false);
  assert.equal(timers.get(1).ms, 1000);
  await timers.get(1).fn();
  assert.equal(publications.length, count, "A timestamp-only change must not repaint or toggle busy");
  assert.equal(publications.at(-1).tasks, previous.tasks);
  tasks = [...tasks, { selector: "/new-stage", state: "running", live: true }];
  await timers.get(1).fn();
  assert.equal(publications.length, count + 1);
  assert.equal(publications.at(-1).tasks.length, 2);
  assert.equal(publications.at(-1).busy, false);
  tasks = tasks.map((task) => ({ ...task, state: "completed", live: false }));
  await timers.get(1).fn();
  assert.equal(timers.get(1).ms, 2000);
  model.dispose();
});

test("background updates preserve selection when new tasks push its row off the page", async () => {
  const { createMonitor } = await client();
  let tasks = [{ selector: "/stage", state: "running" }];
  let value;
  let timer;
  let detailReads = 0;
  const model = createMonitor({
    sessionId: "one", publish: (state) => { value = state; },
    schedule: (fn) => { timer = fn; return 1; }, cancel() {},
    fetch: async (url) => {
      const params = new URL(url, "http://host/").searchParams;
      if (params.has("selector")) {
        detailReads++;
        return Response.json({ schema_version: 1, observed_at: "now", project: "/project",
          selector: "/stage", detail_status: "not_available", live: true });
      }
      return Response.json({ schema_version: 1, observed_at: "now", project: "/project", tasks });
    },
  });
  model.visible(true);
  await tick();
  model.select("/stage");
  await tick();
  const before = detailReads;
  await timer();
  assert.equal(detailReads, before, "Unchanged row must not recollect detail on every background read");
  tasks = [{ selector: "/new-stage", state: "running" }];
  await timer();
  assert.equal(value.selected, "/stage");
  assert.equal(value.detail.selector, "/stage");
  assert.equal(detailReads, before + 1, "Off-page selected tasks must still be observed");
  model.dispose();
});
