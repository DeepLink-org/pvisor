import assert from "node:assert/strict";
import test from "node:test";
import { documentationCases } from "./documentation.mjs";

test("documented executable cases exist and agree in both languages", async () => {
  const cases = await documentationCases();
  assert.equal(cases.spawn.timeout_seconds, 120);
  assert.deepEqual(cases.spawn.argv.slice(0, 2), ["sh", "-c"]);
  assert.equal(cases.pi.length, 4);
  assert.equal(cases.handoff.trim().split("\n").length, 5);
});
