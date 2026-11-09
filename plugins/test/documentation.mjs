import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

/** Execute the checked-in user example, rather than a second handwritten task. */
export async function documentationCases() {
  const sources = await Promise.all(["en", "zh"].map((locale) =>
    readFile(new URL(`../../docs/src/${locale}/guides/agents/plugins.md`, import.meta.url), "utf8"),
  ));
  const cases = sources.map((source) => {
    const blocks = [...source.matchAll(/^```(\w+)\n([\s\S]*?)^```/gm)];
    const tasks = blocks.filter((block) => block[1] === "json").map((block) => JSON.parse(block[2]))
      .filter((value) => Array.isArray(value.argv) && value.selector === undefined);
    assert.equal(tasks.length, 1, "Expected one documented spawn task");
    const forks = blocks.filter((block) => block[1] === "json").map((block) => JSON.parse(block[2]))
      .filter((value) => value.selector === "PARENT_SELECTOR" && Array.isArray(value.argv));
    const exports = blocks.filter((block) => block[1] === "json").map((block) => JSON.parse(block[2]))
      .filter((value) => value.selector === "CHILD_SELECTOR" && Array.isArray(value.paths) && value.output);
    assert.equal(forks.length, 1, "Expected one documented workspace fork");
    assert.equal(exports.length, 1, "Expected one documented artifact export");
    const experiments = blocks.filter((block) => block[1] === "json").map((block) => JSON.parse(block[2]))
      .filter((value) => Array.isArray(value.tasks));
    assert.equal(experiments.length, 1, "Expected one documented fanout plan");
    const handoffs = blocks.filter((block) => block[1] === "sh" && block[2].startsWith('pvisor status "$stage"'));
    assert.equal(handoffs.length, 1, "Expected one native CLI handoff example");
    const commands = blocks.filter((block) => block[1] === "text" && block[2].startsWith("/pvisor status "));
    assert.equal(commands.length, 1, "Expected one Pi handoff example");
    const runtimes = blocks.filter((block) => block[1] === "sh" && block[2].startsWith("pvisor --version"));
    assert.equal(runtimes.length, 1, "Expected a separately installed native CLI check");
    return { spawn: tasks[0], fork: forks[0], export: exports[0], fanout: experiments[0], handoff: handoffs[0][2], pi: commands[0][2].trim().split("\n"), runtime: runtimes[0][2] };
  });
  assert.deepEqual(cases[0], cases[1], "English and Chinese executable cases must agree");
  return cases[0];
}
