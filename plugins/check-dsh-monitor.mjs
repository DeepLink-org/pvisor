import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import path from "node:path";
import vm from "node:vm";
import { Backend } from "./backend.mjs";

// Optional integration check against a disposable *installed* DSH and packed
// plugin. No user DSH configuration, provider credentials or model calls.
const [modules] = process.argv.slice(2);
if (!modules || !process.env.PVISOR_PLUGIN_BINARY) {
  throw new Error("Usage: PVISOR_PLUGIN_BINARY=/absolute/pvisor node plugins/check-dsh-monitor.mjs /disposable/node_modules\nInstall DSH 0.2.2-rc.1, the packed plugin, React 18.3.1 and react-test-renderer 18.3.1 there first.");
}
const root = await mkdtemp(path.join(tmpdir(), "pvisor-dsh-monitor-"));
const installed = path.join(path.resolve(modules), "pvisor-agent-plugins");
const require = createRequire(path.join(path.resolve(modules), "react/package.json"));
const React = require("react");
const { act, create } = require("react-test-renderer");
for (const name of ["package.json", "dsh.mjs", "dsh-monitor.mjs", "dsh-client.js"]) {
  assert.equal(await readFile(path.join(installed, name), "utf8"),
    await readFile(new URL(name, import.meta.url), "utf8"), `Repack/reinstall the current plugin: ${name} differs`);
}
const project = path.join(root, "project");
const stateDir = path.join(root, "state");
await mkdir(project);
const backend = new Backend(project, { binary: process.env.PVISOR_PLUGIN_BINARY, stateDir, execution: "host" });
let child;
let run;
let rendered;
try {
  run = await backend.run({ argv: ["sh", "-c", "mkdir src; printf candidate > src/a; printf 'monitor output\\n'"] },
    undefined, { name: "候选修复" });
  assert.equal(run.exit_code, 0, JSON.stringify(run));
  // Resolve the real published bundle's package name from the patch's owning
  // tree. An absolute dsh.mjs fixture hides package-subpath discovery failures.
  await symlink(path.resolve(modules), path.join(root, "node_modules"), "dir");
  const fixture = path.join(root, "session.mjs");
  await writeFile(fixture, `export const inject=["sessions"];
export function apply(ctx) {
 ctx.sessions.create("pvisor-monitor-case",{meta:{cwd:${JSON.stringify(project)}}});
}`);
  const patch = path.join(root, "patch.yml");
  const shippedPatch = await readFile(path.join(installed, "cordis.patch.yml"), "utf8");
  await writeFile(patch, `${shippedPatch}
- insert:
    - id: pvisor-monitor-case
      name: ${JSON.stringify(fixture)}
`);
  child = spawn(process.execPath, [
    path.join(path.resolve(modules), "@deepseek-ai/dsh/lib/bin.js"),
    "web", "--patch", patch, "--port", "0", "--no-open",
  ], { cwd: project, env: {
    PATH: process.env.PATH, HOME: root, DSH_HOME: path.join(root, "dsh"),
    DSH_TELEMETRY_MODE: "DISABLED", PVISOR_PLUGIN_BINARY: process.env.PVISOR_PLUGIN_BINARY,
    PVISOR_PLUGIN_EXECUTION: "host",
    PVISOR_PLUGIN_STATE_DIR: stateDir, NO_COLOR: "1",
  }, stdio: ["ignore", "pipe", "pipe"] });
  let logs = "";
  const url = await new Promise((resolve, reject) => {
    const deadline = setTimeout(() => reject(new Error("DSH startup timed out.")), 30_000);
    const read = (chunk) => {
      logs += chunk;
      const found = logs.match(/dsh web: (http:\/\/127\.0\.0\.1:\d+\/\S+)/);
      if (found) { clearTimeout(deadline); resolve(found[1]); }
    };
    child.stdout.on("data", read);
    child.stderr.on("data", read);
    child.once("error", (error) => { clearTimeout(deadline); reject(error); });
    child.once("exit", (code) => {
      clearTimeout(deadline);
      reject(new Error(`DSH exited before ready (${code}); ${logs.replace(/https?:\/\/\S+/g, "[URL redacted]")}`));
    });
  });
  // The startup token/cookie stays in memory, never in logs or artifacts.
  const initial = await fetch(url, { redirect: "manual" });
  const cookie = initial.headers.get("set-cookie")?.split(";")[0];
  assert.ok(cookie, "DSH must exchange its startup token for an authenticated browser cookie");
  const base = new URL("/", url);
  const headers = { cookie };
  const index = await (await fetch(base, { headers })).text();
  const boot = index.match(/globalThis\["__DSH_BOOT__"\]\s*=\s*([\s\S]*?)<\/script>/);
  assert.ok(boot, "DSH must compose the browser boot graph");
  const graph = JSON.parse(boot[1].replace(/;\s*$/, ""));
  const entry = graph.entries.find((row) => row.id === "pvisor-agent-plugins");
  assert.ok(entry, "Packed plugin must be discovered from its Host Loader entry");
  const script = await (await fetch(new URL(entry.url, base), { headers })).text();
  assert.match(script, /pvisor-agent-plugins/);
  assert.match(script, /pvisor-monitor/);
  const route = new URL("api/pvisor/monitor?session=pvisor-monitor-case", base);
  const unauthenticated = await fetch(route);
  assert.ok([401, 403].includes(unauthenticated.status), "Anonymous monitor access must be refused by DSH");
  const foreignOrigin = await fetch(route, { headers: { ...headers, origin: "https://untrusted.example" } });
  assert.equal(foreignOrigin.status, 403);
  const pageReply = await fetch(route, { headers });
  assert.equal(pageReply.status, 200, await pageReply.clone().text());
  const page = await pageReply.json();
  assert.ok(page.tasks.some((task) => task.selector === run.selector));
  route.searchParams.set("selector", run.selector);
  const detail = await (await fetch(route, { headers })).json();
  assert.equal(detail.detail_status, "complete", JSON.stringify(detail));
  assert.equal(detail.counts.file, 1);
  assert.equal(detail.counts.directory, 1);
  const fileDiff = detail.files.find((file) => file.path === "src/a");
  assert.equal(fileDiff.diff_status, "text", JSON.stringify(detail.files));
  assert.equal(fileDiff.additions, 1);
  assert.equal(fileDiff.deletions, 0);
  route.searchParams.set("view", "output");
  const output = await (await fetch(route, { headers })).json();
  assert.equal(output.stdout, "monitor output\n");

  // Render the packed browser module with real React hooks. This is component
  // acceptance, not a claim of Electron or end-user browser interaction.
  const document = { visibilityState: "visible", addEventListener() {}, removeEventListener() {},
    querySelectorAll: () => [] };
  const facade = { mode: "queue", pendingQueue: [], load(value) { this.pendingQueue.push(value); } };
  const browser = { fetch: async (url) => {
    const params = new URL(url, base).searchParams;
    return Response.json(params.has("selector") ? params.get("view") === "output" ? output : detail : page);
  }, __ModuleLoader__: facade };
  const sandbox = vm.createContext({
    window: browser, document, AbortController, URL, URLSearchParams, setTimeout, clearTimeout,
  });
  // Use DSH's real browser loader, not a hand-written factory invocation.
  vm.runInContext(await readFile(path.join(path.resolve(modules),
    "@deepseek-ai/dsh-client-modules/lib/client.js"), "utf8"), sandbox);
  const bootstrapRow = facade.pendingQueue.find((row) => row.id === "@deepseek-ai/dsh-client-modules");
  const bootstrap = bootstrapRow.factory(() => { throw new Error("Unexpected bootstrap dependency"); });
  facade.pendingQueue = facade.pendingQueue.filter((row) => row !== bootstrapRow);
  // The full browser shell seeds these styled atoms. Node's component check
  // supplies DOM-equivalent fixtures; it does not verify the browser CSS.
  const primitives = {
    Button: ({ variant, size, ...props }) => React.createElement("button", props),
    Pill: ({ active, ...props }) => React.createElement("button", { ...props, "aria-pressed": active }),
    Tag: ({ tone, children }) => React.createElement("span", { "data-tone": tone }, children),
  };
  const loader = bootstrap.createClientModuleSystem(facade, {
    id: bootstrapRow.id, exports: bootstrap,
  }, {
    boot: graph, staticModules: { react: React, "@deepseek-ai/dsh-client-ui-primitives": primitives },
    loadBundle: async (url) => {
      const source = await (await fetch(new URL(url, base), { headers })).text();
      vm.runInContext(source, sandbox);
    },
  });
  const plugin = await loader.import("pvisor-agent-plugins");
  let Body;
  plugin.apply({
    effect: (fn) => fn(), sidebarRightTabs: { register: () => () => {} },
    slots: { inject: (_name, fn) => fn(), register: (_spec, component) => { Body = component; return () => {}; } },
  });
  const tab = { visible: true, signal: new AbortController().signal,
    actions: { bindCommands: () => () => {} } };
  await act(async () => {
    rendered = create(React.createElement(React.StrictMode, null,
      React.createElement(Body, { sessionId: "pvisor-monitor-case", useTabInfo: () => ({ tab }) })));
  });
  const text = () => JSON.stringify(rendered.toJSON());
  assert.match(text(), /只读/);
  const taskButton = rendered.root.findAllByType("button").find((button) => button.props.title === run.selector);
  assert.ok(taskButton);
  assert.equal(taskButton.findAll((node) => node.props["data-tone"]).length, 1, "Only one primary status per run");
  assert.match(text(), /候选修复/);
  await act(async () => { taskButton.props.onClick(); });
  assert.match(text(), /src\/a/);
  assert.match(text(), /目录/);
  assert.match(text(), /\+1/);
  assert.match(text(), /\+candidate/);
  assert.ok(!text().includes("项目内"));
  assert.ok(!rendered.root.findAllByType("button").some((button) => button.children.includes("文本 Diff")),
    "File diffs must be inline rather than hidden behind a separate task tab");
  await act(async () => {
    rendered.root.findAllByType("button").find((button) => button.children.includes("终态输出")).props.onClick();
  });
  assert.match(text(), /monitor output/);
  assert.ok(!rendered.root.findAllByType("button").some((button) =>
    /^(apply|drop|fork|export)$/i.test(button.children.join(""))));
  await act(async () => { rendered.unmount(); });
  rendered = null;
  console.log(JSON.stringify({
    dsh: "0.2.2-rc.1", plugin: JSON.parse(await readFile(path.join(installed, "package.json"), "utf8")).version,
    checks: ["packed Host activation", "browser boot graph, served bundle and real DSH module loader", "anonymous/foreign-origin refusal",
      "authenticated real CLI task/candidate/output reads", "real React task selection and output rendering"],
    scope: "No model calls, user DSH configuration, Electron or interactive browser/CSS acceptance. Styled atoms use DOM-equivalent fixtures in the Node component check.",
  }, null, 2));
} finally {
  if (rendered) await act(async () => { rendered.unmount(); });
  if (child && child.exitCode === null && child.signalCode === null) {
    const closed = once(child, "close");
    child.kill("SIGTERM");
    const deadline = setTimeout(() => child.kill("SIGKILL"), 5000);
    await closed;
    clearTimeout(deadline);
  }
  let discarded = !run;
  if (run) {
    const result = await backend.operate("drop", { selector: run.selector }).catch(() => null);
    discarded = result?.exit_code === 0 && !result.interrupted;
  }
  if (discarded) await rm(root, { recursive: true, force: true });
  else console.error(`Native cleanup was not confirmed; isolated test evidence retained at ${root}`);
}
