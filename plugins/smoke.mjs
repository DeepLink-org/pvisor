import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { Backend } from "./backend.mjs";
import { documentationCases } from "./test/documentation.mjs";

const root = await mkdtemp(path.join(tmpdir(), "pvisor-plugin-smoke-"));
try {
  const project = path.join(root, "project");
  await mkdir(project);
  const backend = new Backend(project, { stateDir: path.join(root, "state") });
  const run = await backend.run((await documentationCases()).spawn);
  assert.equal(run.exit_code, 0, JSON.stringify(run));
  assert.equal(run.interrupted, false);
  await assert.rejects(stat(path.join(project, "accepted.txt")), { code: "ENOENT" });
  const review = await backend.operate("review", { selector: run.selector });
  assert.equal(review.exit_code, 0, JSON.stringify(review));
  assert.match(review.stdout, /accepted\.txt/);
  const input = { selector: run.selector, paths: ["accepted.txt"] };
  const preview = await backend.operate("preview", input);
  assert.equal(preview.exit_code, 0, JSON.stringify(preview));
  await assert.rejects(stat(path.join(project, "accepted.txt")), { code: "ENOENT" });
  const apply = await backend.operate("apply", input);
  assert.equal(apply.exit_code, 0, JSON.stringify(apply));
  assert.equal(await readFile(path.join(project, "accepted.txt"), "utf8"), "candidate\n");
  const drop = await backend.operate("drop", { selector: run.selector });
  assert.equal(drop.exit_code, 0, JSON.stringify(drop));
  await assert.rejects(stat(path.join(project, "discarded.txt")), { code: "ENOENT" });
  assert.equal(await readFile(path.join(project, "accepted.txt"), "utf8"), "candidate\n");
  console.log("pVisor plugin native smoke passed: staging, review, selective apply, drop");
  await rm(root, { recursive: true });
} catch (error) {
  console.error(`Smoke failed; retained project and state: ${root}`);
  console.error(error);
  process.exitCode = 1;
}
