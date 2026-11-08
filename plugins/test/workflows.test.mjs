import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { Backend } from "../backend.mjs";
import { rustEnvironment } from "../environment.mjs";
import { fanout } from "../fanout.mjs";
import { registerDsh } from "../dsh.mjs";
import registerPi from "../pi.mjs";
import { callTool, failed } from "../tools.mjs";

async function fixture(t) {
  const root = await realpath(await mkdtemp(path.join(tmpdir(), "pvisor-workflows-")));
  t.after(() => rm(root, { recursive: true, force: true }));
  const project = path.join(root, "project");
  await mkdir(project);
  const exportDir = path.join(root, "artifacts");
  await mkdir(exportDir, { mode: 0o700 });
  const config = path.join(root, "fixture.json");
  await writeFile(config, "{}");
  const binary = path.join(root, "pvisor");
  await writeFile(binary, `#!${process.execPath}
import {readFileSync,writeFileSync,mkdirSync,appendFileSync,existsSync} from "node:fs";
const args=process.argv.slice(2);
const config=JSON.parse(readFileSync(${JSON.stringify(config)},"utf8"));
if(args.includes("--help")){console.log("--safe --no-agent-defaults --stage --timeout --overlaynet-deny-all --path --dry-run --json "+(config.lifecycle?"--lifecycle --session ":""));process.exit(0);}
const log=${JSON.stringify(path.join(root, "calls.jsonl"))};
appendFileSync(log,JSON.stringify({args,env:{HOME:process.env.HOME,CARGO_HOME:process.env.CARGO_HOME}})+"\\n");
if(args[0]==="--version"){console.log("pvisor fixture");process.exit(0);}
if(args[0]==="vm-check"){
 console.log(JSON.stringify(config.invalid_vm?{}:{schema_version:1,operation:"vm-check",
  backend:"kvm",can_create_vm:config.vm_available??false,firmware:config.vm_firmware??"embedded",
  boot_verified:false,reason:config.vm_available?"KVM_CREATE_VM succeeded":"KVM permission denied"}));
 process.exit(0);
}
let stage=args[1];
if(args[0]==="spawn"||args[0]==="fork"){
 if(config.vm_launch_failure&&args.includes("vm")){console.error("VM boot failed");process.exit(1);}
 if(args[0]==="fork"&&config.fork_refuse){console.error("workspace fork refused");process.exit(1);}
 stage=args[args.indexOf("--stage")+1];mkdirSync(stage,{recursive:true});
 writeFileSync(stage+"/result",args.includes("false")?"7":"0");
 console.log("native launch diagnostic");
 process.exit(args.includes("false")?7:0);
}
const code=Number(readFileSync(stage+"/result","utf8"));
if(args[0]==="status"&&!args.includes("--review")&&config.slow_status===stage){
 await new Promise(resolve=>setTimeout(resolve,3000));
}
if(args[0]==="status"&&args.includes("--review")){
 if(config.fault==="review-error"){console.error("invalid completion");process.exit(1);}
 const out={stdout:"hello task\\n",stderr:"task warning\\n",stdout_truncated:false,stderr_truncated:false,...config.output};
 console.log(JSON.stringify({schema_version:config.fault==="old-schema"?5:6,
 filesystem:{target:config.fault==="foreign"?"/foreign":process.cwd()},
 safety:{filesystem_non_bypassable:!config.unsafe_source,network_non_bypassable:true},
 network:{policy:{capability:{mode:"deny"}}},
 run:{run_id:"session",execution_id:"execution",state:config.fault==="live"?"running":code?"failed":"completed",
 executor:{kind:config.non_host?"virtual_machine":"process"},
 exit_code:config.omit_exit_code?undefined:code,finished_at_unix_ms:1,output:out}}));
}else if(args[0]==="status"){
 if(config.invalid_status){console.log("invalid status");process.exit(0);}
 console.log(JSON.stringify({schema_version:config.lifecycle?1:3,
 detail:config.lifecycle?"lifecycle":undefined,exec_supported:config.exec_supported??false,
 state:config.state||"completed",live:config.live??false,
 run:{stage,run_id:"session",execution_id:"execution",overlay:{target:process.cwd(),generation:1,
 state:config.applied?"applied":existsSync(stage+"/dropped")?"dropped":"staged"}},
 filesystem:{changed_files:config.changed_files??1}}));
}else if(args[0]==="drop"){
 if(config.refuse===stage||config.drop_refuse){console.error("native drop refused");process.exit(1);}
 writeFileSync(stage+"/dropped","yes");console.log("dropped");
}else if(args[0]==="export"){
 if(config.export_refuse){console.error("export refused");process.exit(1);}
 const destination=args[args.indexOf("--output")+1];mkdirSync(destination,{mode:0o700});
 console.log(JSON.stringify(config.bad_manifest?{}:{schema_version:1,
  destination:config.foreign_destination?"/foreign":destination,project_root:process.cwd(),
  session_id:"session",execution_id:"execution",state:code?"failed":"completed",
  partial_results:code!==0,exit_code:code,entries:[],deletions:[]}));
}else if(args[0]==="apply"&&args.includes("--dry-run")){
 if(args.includes("--json")){
  if(config.proof_empty&&args.includes("--all")){
   console.log(JSON.stringify({schema_version:1,operation:"apply",mode:"preview",
    status:config.proof_pending?"blocked":"ready",scope:{kind:"project",target:process.cwd()},
    source:{stage,session_id:"session",generation:config.generation_changed?2:1},
    selected:[],required:[],candidates:[],blockers:config.proof_pending?["pending apply"]:[],
    remaining:{in_scope:0,outside_scope:0},execution:null}));
   process.exit(config.proof_pending?3:0);
  }
  const countFile=stage+"/preview-count";
  const count=existsSync(countFile)?Number(readFileSync(countFile,"utf8"))+1:1;
  writeFileSync(countFile,String(count));
  console.log(JSON.stringify({schema_version:1,operation:"apply",mode:"preview",status:"ready",
   source:{stage},scope:{kind:"project",target:process.cwd()},
   selected:[{path:args[args.indexOf("--path")+1],kind:"modified",size_bytes:14}],required:[],
   candidates:[],blockers:[],remaining:{in_scope:0,outside_scope:0},
   plan:config.plan_change?String(count):"stable-plan"}));
 }else{
  if(config.detail_failure){console.error("diff failed");process.exit(1);}
  console.log("--- chosen.txt\\n+++ chosen.txt\\n@@ -1 +1 @@\\n-old\\n+new");
 }
}else console.log("{}");
`, { mode: 0o700 });
  const backend = new Backend(project, { binary, stateDir: path.join(root, "state"), exportDir, execution: "host" });
  return {
    root, project, exportDir, backend,
    configure: (value) => writeFile(config, JSON.stringify(value)),
    calls: async () => (await readFile(path.join(root, "calls.jsonl"), "utf8")).trim().split("\n").map(JSON.parse),
  };
}

test("terminal preparation requires live native exec capability and never launches a shell itself", async (t) => {
  const f = await fixture(t);
  const run = await f.backend.run({ argv: ["true"] });
  await assert.rejects(f.backend.terminal({ selector: run.selector }), /CAPABILITY_UNSUPPORTED/);
  await f.configure({ lifecycle: true, state: "running", live: true, exec_supported: true });
  const prepared = await f.backend.terminal({ selector: run.selector });
  assert.equal(prepared.terminal_status, "ready");
  assert.deepEqual(prepared.argv.slice(1), ["exec", "--session", "-it", run.selector, "/bin/sh"]);
  assert.equal(prepared.session_id, "session");
  assert.ok((await f.calls()).some(({ args }) => args.includes("--lifecycle")));
  assert.ok(!(await f.calls()).some(({ args }) => args[0] === "exec"));
  for (const state of ["suspended", "completed", "unknown"]) {
    await f.configure({ lifecycle: true, state, live: true, exec_supported: true });
    await assert.rejects(f.backend.terminal({ selector: run.selector }), /CAPABILITY_UNSUPPORTED/);
  }
  await f.configure({ lifecycle: true, state: "running", live: true, exec_supported: false });
  await assert.rejects(f.backend.terminal({ selector: run.selector }), /CAPABILITY_UNSUPPORTED/);
});

test("VM-first run selects once, preserves safe staging and never retries boot failure on Host", async (t) => {
  const f = await fixture(t);
  const rootfs = path.join(f.root, "linux-root");
  await mkdir(rootfs, { mode: 0o700 });
  f.backend.execution = "auto";
  f.backend.vmRootfs = rootfs;
  await f.configure({ vm_available: true, vm_launch_failure: true });
  const result = await f.backend.run({ argv: ["sh", "-c", "printf candidate"], timeout_seconds: 9 });
  assert.equal(result.execution.selected, "vm");
  assert.equal(result.exit_code, 1);
  const launches = (await f.calls()).filter(({ args }) => args[0] === "spawn");
  assert.equal(launches.length, 1);
  const args = launches[0].args;
  assert.equal(args[args.indexOf("--executor") + 1], "vm");
  assert.equal(args[args.indexOf("--rootfs") + 1], rootfs);
  for (const flag of ["--safe", "--no-agent-defaults", "--overlaynet-deny-all", "--stage"]) assert.ok(args.includes(flag));
  assert.equal(args[args.indexOf("--timeout") + 1], "9s");
});

test("Doctor exposes blocked VM readiness and an operator-authorized fallback without running user code", async (t) => {
  const f = await fixture(t);
  f.backend.execution = "auto";
  await f.configure({ vm_available: false });
  const blocked = await f.backend.doctor({ probe: true });
  assert.equal(blocked.exit_code, 1);
  assert.equal(blocked.execution.selected, null);
  assert.equal(blocked.probe, undefined);
  assert.ok(!(await f.calls()).some(({ args }) => args[0] === "spawn"));
  await assert.rejects(f.backend.run({ argv: ["true"] }), /Host fallback is disabled/);
  f.backend.allowHostFallback = "true";
  const run = await f.backend.run({ argv: ["true"] });
  assert.equal(run.execution.selected, "host"); assert.equal(run.execution.fallback, true);
  const tasks = await f.backend.tasks();
  assert.equal(tasks.tasks.find((task) => task.selector === run.selector).execution.fallback, true);
  f.backend.execution = "vm";
  await assert.rejects(f.backend.run({ argv: ["true"] }), /not ready/);
});

test("spawn returns captured task streams separately from CLI diagnostics, including failures", async (t) => {
  const f = await fixture(t);
  for (const command of ["true", "false"]) {
    const result = await f.backend.run({ argv: [command] });
    assert.equal(result.stdout, "hello task\n");
    assert.equal(result.stderr, "task warning\n");
    assert.match(result.cli.stdout, /native launch diagnostic/);
    assert.equal(result.output_status, "complete");
    assert.equal(result.exit_code, command === "true" ? 0 : 7);
    assert.equal(result.task_exit_code, result.exit_code);
    assert.equal(failed(result), command === "false");
    const reopened = await callTool(f.backend, "pvisor_output", { selector: result.selector });
    assert.equal(reopened.stdout, result.stdout);
  }
});

test("fanout display names survive task discovery without becoming executable arguments", async (t) => {
  const f = await fixture(t);
  const result = await fanout(f.backend, { tasks: [{ name: "honest-fix", argv: ["true"] }] });
  const selector = result.results[0].result.selector;
  const page = await f.backend.tasks({ order: "newest" });
  assert.equal(page.tasks.find((task) => task.selector === selector).name, "honest-fix");
  assert.equal((await f.backend.operate("status", { selector })).name, "honest-fix");
  const launch = (await f.calls()).find((call) => call.args[0] === "spawn");
  assert.ok(!launch.args.includes("honest-fix"));
  await assert.rejects(f.backend.run({ argv: ["true"] }, undefined, { name: "bad\nname" }), /display name/);
});

test("terminal failure output remains valid when the native optional exit code is absent", async (t) => {
  const f = await fixture(t);
  await f.configure({ omit_exit_code: true });
  const result = await f.backend.run({ argv: ["false"] });
  assert.equal(result.output_status, "complete");
  assert.equal(result.task_state, "failed");
  assert.equal(result.task_exit_code, null);
  assert.equal(failed(result), true);
});

test("Doctor's explicit probe retains its task and propagates unavailable output", async (t) => {
  const f = await fixture(t);
  await assert.rejects(f.backend.doctor({ probe: "yes" }), /boolean/);
  const passive = await f.backend.doctor();
  assert.equal(passive.probe, undefined);
  assert.ok(!(await f.calls()).some((call) => call.args[0] === "spawn"));
  const probed = await f.backend.doctor({ probe: true });
  assert.equal(probed.exit_code, 0);
  assert.ok(probed.probe.selector);
  assert.ok(!(await f.calls()).some((call) => call.args[0] === "drop"));
  await f.configure({ fault: "review-error" });
  const unavailable = await f.backend.doctor({ probe: true });
  assert.equal(unavailable.exit_code, 1);
  assert.equal(unavailable.probe.output_status, "unavailable");
});

test("fork validates ownership/state and launches one workspace branch without fallback", async (t) => {
  const f = await fixture(t);
  const parent = await f.backend.run({ argv: ["true"] });
  for (const extra of [{ timeout_seconds: 600 }, { executor: "vm" }, { checkpoint: "other" }]) {
    await assert.rejects(f.backend.fork({ selector: parent.selector, argv: ["true"], ...extra }), /only/);
  }
  for (const fault of [{ live: true }, { state: "unknown" }, { state: "hibernated" }, { applied: true }]) {
    await f.configure(fault);
    await assert.rejects(f.backend.fork({ selector: parent.selector, argv: ["true"] }), /retained/);
  }
  assert.equal((await f.calls()).filter((call) => call.args[0] === "fork").length, 0);
  for (const fault of [{ unsafe_source: true }, { non_host: true }]) {
    await f.configure(fault);
    await assert.rejects(f.backend.fork({ selector: parent.selector, argv: ["true"] }), /no policy downgrade/);
  }
  const other = path.join(f.root, "other");
  await mkdir(other);
  await assert.rejects(new Backend(other, {
    binary: f.backend.binary, stateDir: f.backend.stateInput,
  }).fork({ selector: parent.selector, argv: ["true"] }), /belong/);
  await f.configure({});
  const child = await callTool(f.backend, "pvisor_fork", { selector: parent.selector, argv: ["echo", "--executor", "vm"] });
  assert.equal(child.exit_code, 0);
  assert.equal(child.parent_selector, parent.selector);
  assert.notEqual(child.selector, parent.selector);
  const calls = await f.calls();
  const sourceCall = calls.find((call) => call.args[0] === "spawn");
  const forkCall = calls.find((call) => call.args[0] === "fork");
  assert.deepEqual(forkCall.args, ["fork", parent.selector, "--state", "workspace", "--stage", child.selector,
    "--", "echo", "--executor", "vm"]);
  assert.notEqual(sourceCall.env.HOME, forkCall.env.HOME);
  await f.configure({ fork_refuse: true });
  const refused = await f.backend.fork({ selector: parent.selector, argv: ["true"] });
  assert.equal(refused.exit_code, 1);
  assert.equal(refused.output_status, "unavailable");
  assert.match(refused.cli.stderr, /workspace fork refused/);
  assert.equal((await f.calls()).filter((call) => call.args[0] === "spawn").length, 1);
});

test("export confines destinations to private operator root and retains uncertain publication", async (t) => {
  const f = await fixture(t);
  const run = await f.backend.run({ argv: ["false"] });
  const input = { selector: run.selector, paths: ["report.txt"], output: "case-a" };
  const disabled = new Backend(f.project, { binary: f.backend.binary, stateDir: f.backend.stateInput, exportDir: "" });
  await assert.rejects(disabled.export(input), /PVISOR_PLUGIN_EXPORT_DIR/);
  for (const output of ["/absolute", "../escape", "nested/path", ".", "..", ".hidden", "x\\y"]) {
    await assert.rejects(f.backend.export({ ...input, output }), /directory name/);
  }
  for (const paths of [[], ["/etc/passwd"], ["../secret"]]) {
    await assert.rejects(f.backend.export({ ...input, paths }), /relative paths/);
  }
  const inside = path.join(f.project, "artifacts");
  await mkdir(inside, { mode: 0o700 });
  await assert.rejects(new Backend(f.project, {
    binary: f.backend.binary, stateDir: f.backend.stateInput, exportDir: inside,
  }).export(input), /disjoint/);
  const alias = path.join(f.root, "project-alias");
  await chmod(f.project, 0o700);
  await symlink(f.project, alias);
  await assert.rejects(new Backend(f.project, {
    binary: f.backend.binary, stateDir: f.backend.stateInput, exportDir: alias,
  }).export(input), /disjoint/);
  await chmod(f.exportDir, 0o755);
  await assert.rejects(f.backend.export(input), /0700/);
  await chmod(f.exportDir, 0o700);
  assert.equal((await f.calls()).filter((call) => call.args[0] === "export").length, 0);
  const result = await callTool(f.backend, "pvisor_export", input);
  assert.equal(result.exit_code, 0);
  assert.equal(result.export_status, "published");
  assert.equal(result.manifest.partial_results, true);
  assert.equal(result.manifest.exit_code, 7);
  assert.equal(failed(result), false, "Export success is not the source task's success");
  const call = (await f.calls()).find((call) => call.args[0] === "export");
  assert.deepEqual(call.args, ["export", run.selector, "--output", path.join(f.exportDir, "case-a"), "--json", "--path=report.txt"]);
  await f.configure({ bad_manifest: true });
  const uncertain = await f.backend.export({ ...input, output: "uncertain" });
  assert.equal(uncertain.export_status, "unconfirmed");
  assert.equal(failed(uncertain), true);
  assert.equal(await realpath(uncertain.destination), uncertain.destination, "Do not delete uncertain artifacts");
  await f.configure({ foreign_destination: true });
  const foreign = await f.backend.export({ ...input, output: "foreign-response" });
  assert.equal(foreign.export_status, "unconfirmed");
  assert.match(foreign.export_error, /scope/);
});

test("captured output is bounded in UTF-8 and preserves runtime truncation", async (t) => {
  const f = await fixture(t);
  await f.configure({ output: { stdout: "界".repeat(150_000), stderr_truncated: true } });
  const result = await f.backend.run({ argv: ["true"] });
  assert.ok(Buffer.byteLength(result.stdout) <= 256 * 1024);
  assert.ok(!result.stdout.endsWith("\uFFFD"));
  assert.equal(result.truncated, true);
  await f.configure({ output: { stdout: "", stderr: "", stdout_truncated: true } });
  assert.equal((await f.backend.output({ selector: result.selector })).truncated, true);
});

test("invalid/unfinished/oversized review never fabricates successful task output", async (t) => {
  const f = await fixture(t);
  for (const fault of ["old-schema", "foreign", "live", "review-error"]) {
    await f.configure({ fault });
    const result = await f.backend.run({ argv: ["true"] });
    assert.equal(result.output_status, "unavailable");
    assert.equal(result.stdout, "");
    assert.match(result.cli.stdout, /native launch/);
    assert.ok(result.scratch_home);
    assert.equal(failed(result), true);
    await assert.rejects(f.backend.output({ selector: result.selector }));
  }
  await f.configure({ output: { stdout: "x".repeat(9 * 1024 * 1024) } });
  const result = await f.backend.run({ argv: ["true"] });
  assert.equal(result.output_status, "unavailable");
  assert.match(result.output_error, /8 MiB/);
});

test("apply preview binds scoped diff to a stable native plan and refuses races or diff failure", async (t) => {
  const f = await fixture(t);
  const run = await f.backend.run({ argv: ["true"] });
  const input = { selector: run.selector, paths: ["chosen.txt"] };
  const valid = await f.backend.operate("preview", input);
  assert.equal(valid.approval_ready, true);
  assert.match(valid.summary, /MODIFY "chosen.txt"/);
  assert.match(valid.summary, /\+new/);
  await f.configure({ plan_change: true });
  const changed = await f.backend.operate("preview", input);
  assert.equal(changed.approval_ready, false);
  assert.equal(changed.preview_state, "unavailable");
  assert.equal(changed.diff, "");
  assert.match(changed.summary, /plan changed/);
  await f.configure({ detail_failure: true });
  const broken = await f.backend.operate("preview", input);
  assert.equal(broken.approval_ready, false);
  assert.match(broken.summary, /text preview failed/);
});

test("output cannot select foreign tasks, record files, or relative paths", async (t) => {
  const f = await fixture(t);
  const run = await f.backend.run({ argv: ["true"] });
  for (const selector of ["last", "../completion.json", path.join(run.selector, "completion.json")]) {
    await assert.rejects(f.backend.output({ selector }));
  }
  const other = path.join(f.root, "other");
  await mkdir(other);
  await assert.rejects(new Backend(other, {
    binary: f.backend.binary, stateDir: f.backend.stateInput,
  }).output({ selector: run.selector }), /belong/);
});

test("task listing is project-scoped, paginated and reflects native drop state", async (t) => {
  const f = await fixture(t);
  const runs = await Promise.all([f.backend.run({ argv: ["true"] }), f.backend.run({ argv: ["true"] })]);
  const other = path.join(f.root, "other");
  await mkdir(other);
  await new Backend(other, { binary: f.backend.binary, stateDir: f.backend.stateInput, execution: "host" }).run({ argv: ["true"] });
  const first = await f.backend.tasks({ limit: 1 });
  assert.equal(first.tasks.length, 1);
  assert.ok(first.next_cursor);
  const second = await f.backend.tasks({ limit: 1, cursor: first.next_cursor });
  assert.equal(second.tasks.length, 1);
  assert.equal(second.next_cursor, null);
  assert.deepEqual([...first.tasks, ...second.tasks].map((item) => item.selector).sort(), runs.map((item) => item.selector).sort());
  await f.backend.operate("drop", { selector: runs[0].selector });
  const listed = await callTool(f.backend, "pvisor_tasks", {});
  assert.equal(listed.tasks.find((item) => item.selector === runs[0].selector).filesystem, "dropped");
  await assert.rejects(f.backend.tasks({ limit: 51 }));
});

test("newest task order is global before pagination, with labeled legacy timestamps", async (t) => {
  const f = await fixture(t);
  const runs = [];
  for (let index = 0; index < 3; index++) runs.push(await f.backend.run({ argv: ["true"] }));
  // Assign timestamps opposite UUID order to ensure this is not a reversed ID sort.
  runs.sort((a, b) => a.selector.localeCompare(b.selector));
  const times = [20, 30, 10];
  for (let index = 0; index < runs.length; index++) {
    const file = path.join(path.dirname(runs[index].selector), "task.json");
    const record = JSON.parse(await readFile(file, "utf8"));
    record.created_at_unix_ms = times[index];
    await writeFile(file, JSON.stringify(record));
  }
  const first = await f.backend.tasks({ order: "newest", limit: 1 });
  assert.equal(first.tasks[0].selector, runs[1].selector);
  assert.equal(first.tasks[0].created_at_source, "adapter");
  const second = await f.backend.tasks({ order: "newest", limit: 1, cursor: first.next_cursor });
  assert.equal(second.tasks[0].selector, runs[0].selector);
  const third = await f.backend.tasks({ order: "newest", limit: 1, cursor: second.next_cursor });
  assert.equal(third.tasks[0].selector, runs[2].selector);
  assert.equal(third.next_cursor, null);
  await assert.rejects(f.backend.tasks({ order: "random" }));
  await assert.rejects(f.backend.tasks({ order: "newest", cursor: "11111111-1111-4111-8111-111111111111" }), /cursor/);
  const file = path.join(path.dirname(runs[0].selector), "task.json");
  const legacy = JSON.parse(await readFile(file, "utf8"));
  delete legacy.created_at_unix_ms;
  await writeFile(file, JSON.stringify(legacy));
  const page = await f.backend.tasks({ order: "newest" });
  const old = page.tasks.find((item) => item.selector === runs[0].selector);
  assert.ok(["record_birthtime", "record_mtime"].includes(old.created_at_source));
  assert.ok(old.created_at_unix_ms > 0);
});

test("a slow status becomes unknown without preventing other task rows from updating", async (t) => {
  const f = await fixture(t);
  const runs = await Promise.all(Array.from({ length: 3 }, () => f.backend.run({ argv: ["true"] })));
  await f.configure({ slow_status: runs[0].selector });
  const page = await f.backend.tasks({ order: "newest" });
  assert.equal(page.tasks.find((task) => task.selector === runs[0].selector).state, "unknown");
  for (const run of runs.slice(1)) {
    assert.equal(page.tasks.find((task) => task.selector === run.selector).state, "completed");
  }
});

test("malformed task records are not listed or accepted as selectors", async (t) => {
  const f = await fixture(t);
  const run = await f.backend.run({ argv: ["true"] });
  await writeFile(path.join(path.dirname(run.selector), "task.json"), "null");
  assert.deepEqual((await f.backend.tasks()).tasks, []);
  await assert.rejects(f.backend.prepareDrop({ selectors: [run.selector] }), /unsupported schema/);
});

test("batch drop validates all selections before any mutation and reports partial outcomes", async (t) => {
  const f = await fixture(t);
  const a = await f.backend.run({ argv: ["true"] });
  const b = await f.backend.run({ argv: ["true"] });
  await assert.rejects(f.backend.dropBatch({ selectors: [a.selector, "last"] }));
  assert.ok(!(await f.calls()).some((call) => call.args[0] === "drop"));
  await f.configure({ refuse: b.selector });
  const result = await f.backend.dropBatch({ selectors: [a.selector, a.selector, b.selector] });
  assert.deepEqual(result.results.map((item) => item.outcome), ["dropped", "refused"]);
  assert.equal(result.exit_code, 1);
  assert.ok(await readFile(path.join(path.dirname(a.selector), "task.json")));
  const controller = new AbortController();
  controller.abort();
  const cancelled = await f.backend.dropBatch({ selectors: [b.selector] }, controller.signal);
  assert.equal(cancelled.results[0].outcome, "not_attempted");
});

test("already-applied drop is a benign no-op only with terminal empty native evidence and clean preview", async (t) => {
  const f = await fixture(t);
  const run = await f.backend.run({ argv: ["true"] });
  const applied = { applied: true, changed_files: 0, proof_empty: true, drop_refuse: true };
  await f.configure(applied);
  const result = await f.backend.operate("drop", { selector: run.selector });
  assert.equal(result.exit_code, 0);
  assert.equal(result.native_exit_code, 1);
  assert.equal(result.status, "already_applied");
  assert.equal(result.noop, true);
  const batch = await f.backend.dropBatch({ selectors: [run.selector] });
  assert.equal(batch.exit_code, 0);
  assert.equal(batch.results[0].outcome, "already_applied");
  for (const fault of [
    { changed_files: 1 }, { state: "unknown" }, { state: "hibernated" },
    { live: true }, { proof_pending: true }, { invalid_status: true }, { generation_changed: true },
  ]) {
    await f.configure({ ...applied, ...fault });
    const refused = await f.backend.operate("drop", { selector: run.selector });
    assert.equal(refused.exit_code, 1, JSON.stringify(fault));
    assert.equal(refused.status, undefined);
    assert.match(refused.stderr, /native drop refused/);
  }
});

test("DSH single and batch drops never request approval, but still require context and respect cancellation", async () => {
  const definitions = new Map();
  let prompts = 0;
  let drops = 0;
  let controller = new AbortController();
  const backend = {
    project: async () => "/project",
    operate: async (action) => { assert.equal(action, "drop"); drops++; return { exit_code: 0 }; },
    dropBatch: async () => { drops++; return { exit_code: 1, results: [{ outcome: "refused" }] }; },
  };
  registerDsh({ tools: { register: (tool) => definitions.set(tool.name, tool) },
    approval: { request: async () => { prompts++; throw new Error("Drop must not request approval"); } },
  }, (tool) => tool, () => backend);
  const args = { selectors: ["/stage-a", "/stage-b"] };
  const exec = () => ({ agent: { session: { header: { cwd: "/project" } } }, signal: controller.signal });
  const drop = definitions.get("pvisor_drop_batch");
  await definitions.get("pvisor_drop").execute({ selector: "/stage-a" }, exec());
  const result = await drop.execute(args, exec());
  assert.equal(prompts, 0);
  assert.equal(drops, 2);
  assert.equal(result.results[0].outcome, "refused", "Do not lose partial reports in host error handling");
  controller.abort();
  await assert.rejects(drop.execute(args, exec()));
  await assert.rejects(definitions.get("pvisor_drop").execute({ selector: "/stage-a" }, exec()));
  assert.equal(drops, 2);
  await assert.rejects(drop.execute(args, { signal: new AbortController().signal }), /initiating/);
  await assert.rejects(callTool({}, "pvisor_drop_batch", args), /Unknown tool/);
});

test("Pi batch drop works without an interactive confirmation or UI", async () => {
  const commands = new Map();
  let drops = 0;
  let prompts = 0;
  const backend = {
    project: async () => "/project",
    dropBatch: async () => { drops++; return { exit_code: 0 }; },
  };
  registerPi({ registerTool() {}, registerCommand: (name, value) => commands.set(name, value), on() {} },
    { createBackend: () => backend });
  const ctx = { cwd: "/project", hasUI: false,
    ui: { confirm: async () => { prompts++; return true; }, notify() {} } };
  await commands.get("pvisor").handler('drop-batch {"selectors":["/stage"]}', ctx);
  assert.equal(drops, 1);
  ctx.hasUI = true;
  await commands.get("pvisor").handler('drop-batch {"selectors":["/stage"]}', ctx);
  assert.equal(drops, 2);
  assert.equal(prompts, 0);
});

test("Rust environment rejects project shares, temporary paths and model-supplied configuration", async (t) => {
  const f = await fixture(t);
  for (const requested of ["relative", f.project, f.root, "/"]) {
    await assert.rejects(rustEnvironment(requested, f.project, f.backend.stateInput));
  }
  // Validation does not need to create a real toolchain outside the test workspace.
  assert.equal(await rustEnvironment(undefined, f.project, f.backend.stateInput), undefined);
  await assert.rejects(f.backend.run({ argv: ["true"], rust_toolchain: "/opt/rust" }), /only/);
});

test("fanout validates all inputs, bounds concurrency, retains selectors and never retries or mutates", async () => {
  let active = 0;
  let peak = 0;
  const calls = [];
  const backend = {
    doctor: async () => ({ exit_code: 0 }),
    run: async ({ argv }) => {
      calls.push(argv[0]);
      active++;
      peak = Math.max(peak, active);
      await new Promise((resolve) => setTimeout(resolve, 5));
      active--;
      if (argv[0] === "lost") throw Object.assign(new Error("lost completion"), { selector: "/lost/stage" });
      return { exit_code: argv[0] === "false" ? 7 : 0, selector: `/${argv[0]}/stage`, output_status: "complete" };
    },
  };
  const tasks = ["true", "false", "lost"].map((name) => ({ name, argv: [name] }));
  await assert.rejects(fanout(backend, { tasks: [...tasks, { name: "bad", argv: [] }] }));
  assert.deepEqual(calls, []);
  const result = await fanout(backend, { tasks, concurrency: 2 });
  assert.equal(peak, 2);
  assert.deepEqual(calls, ["true", "false", "lost"]);
  assert.deepEqual(result.results.map((item) => item.outcome), ["completed", "needs_review", "unknown"]);
  assert.equal(result.results[2].selector, "/lost/stage");
  assert.deepEqual(result.elapsed_order, ["true"]);
  assert.equal(result.exit_code, 1);
});

test("fanout cancellation retains started results and does not start queued experiments", async () => {
  const controller = new AbortController();
  const calls = [];
  const backend = {
    doctor: async () => ({ exit_code: 0 }),
    run: async ({ argv }) => {
      calls.push(argv);
      controller.abort();
      return { exit_code: 1, interrupted: true, selector: "/known/stage", output_status: "unavailable" };
    },
  };
  const result = await fanout(backend, { concurrency: 1, tasks: [
    { name: "started", argv: ["true"] }, { name: "queued", argv: ["false"] },
  ] }, controller.signal);
  assert.equal(calls.length, 1);
  assert.equal(result.results[0].result.selector, "/known/stage");
  assert.equal(result.results[1].outcome, "not_started");
  assert.equal(result.interrupted, true);
});
