import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { registerDsh } from "./dsh.mjs";
import { tools } from "./tools.mjs";

const [piDirectory, dshDirectory] = process.argv.slice(2);
if (!piDirectory || !dshDirectory || process.argv.length !== 4) {
  console.error("Usage: node check-hosts.mjs /path/to/pi-coding-agent /path/to/dsh-tools\nUse disposable host dependencies; no Agent credentials or UI are needed.");
  process.exit(2);
}
const piRoot = path.resolve(piDirectory);
const dshRoot = path.resolve(dshDirectory);
const piPackage = JSON.parse(await readFile(path.join(piRoot, "package.json"), "utf8"));
const dshPackage = JSON.parse(await readFile(path.join(dshRoot, "package.json"), "utf8"));
assert.equal(piPackage.name, "@earendil-works/pi-coding-agent");
assert.equal(dshPackage.name, "@deepseek-ai/dsh-tools");

const { loadExtensions } = await import(pathToFileURL(path.join(piRoot, "dist/core/extensions/loader.js")));
const requirePi = createRequire(path.join(piRoot, "package.json"));
// pi-ai is ESM-only: require.resolve cannot use its "import" export. Locate
// its metadata through Node's package search paths without loading a CJS entry.
let aiDirectory;
for (const search of requirePi.resolve.paths("@earendil-works/pi-ai")) {
  const packageRoot = path.join(search, "@earendil-works/pi-ai");
  try {
    const metadata = JSON.parse(await readFile(path.join(packageRoot, "package.json"), "utf8"));
    assert.equal(metadata.name, "@earendil-works/pi-ai");
    aiDirectory = path.dirname(path.join(packageRoot, metadata.exports["."].import));
    break;
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
}
assert.ok(aiDirectory, "Pi's pi-ai dependency must be installed");
const { validateToolArguments } = await import(pathToFileURL(path.join(aiDirectory, "utils/validation.js")));
const loaded = await loadExtensions([fileURLToPath(new URL("./pi.mjs", import.meta.url))], process.cwd());
assert.deepEqual(loaded.errors, []);
assert.equal(loaded.extensions.length, 1);
const piTools = [...loaded.extensions[0].tools.values()].map((tool) => tool.definition);
assert.deepEqual(piTools.map((tool) => tool.name), tools.map((tool) => tool.name));
assert.deepEqual([...loaded.extensions[0].commands.keys()], ["pvisor"]);
const spawn = piTools.find((tool) => tool.name === "pvisor_spawn");
assert.deepEqual(validateToolArguments(spawn, { name: spawn.name, arguments: { argv: ["true"] } }).argv, ["true"]);
assert.throws(() => validateToolArguments(spawn, { name: spawn.name, arguments: { argv: [] } }));

const { defineTool } = await import(pathToFileURL(path.join(dshRoot, "lib/index.js")));
const dshTools = [];
registerDsh({ tools: { register: (tool) => dshTools.push(tool) }, approval: {} }, defineTool);
assert.deepEqual(dshTools.map((tool) => tool.name), [...tools.map((tool) => tool.name), "pvisor_apply", "pvisor_drop"]);
// DSH cannot represent numeric bounds in its DSL; shared backend validation
// must still reject them before any native process is launched.
await assert.rejects(
  dshTools.find((tool) => tool.name === "pvisor_spawn").execute(
    { argv: ["true"], timeout_seconds: 0 },
    { agent: { session: { header: { cwd: process.cwd() } } }, signal: new AbortController().signal },
  ),
);
console.log(JSON.stringify({
  pi: { version: piPackage.version, checks: "extension loader, catalog, command registration, argument validation" },
  dsh: { version: dshPackage.version, checks: "defineTool compiler, catalog, backend bounds" },
  scope: "No interactive UI, profile composition, Electron or live-model acceptance",
}, null, 2));
