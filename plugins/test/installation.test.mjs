import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { lstat, mkdir, mkdtemp, readFile, realpath, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";
import { execute } from "../backend.mjs";
import { documentationCases } from "./documentation.mjs";
import { mcpClient } from "./mcp-client.mjs";

test("DOC-PLUGINS-07: packed plugin uses a separately installed runtime and uninstall preserves it", { timeout: 60_000 }, async (t) => {
  const root = await realpath(await mkdtemp(path.join(tmpdir(), "plugin-install-")));
  t.after(() => rm(root, { recursive: true, force: true }));
  const project = path.join(root, "project");
  const prefix = path.join(root, "prefix");
  await mkdir(project);
  const source = fileURLToPath(new URL("../", import.meta.url));
  const plugin = JSON.parse(await readFile(path.join(source, "package.json"), "utf8"));
  assert.equal(plugin.optionalDependencies, undefined);
  assert.equal(plugin.dependencies, undefined);
  assert.equal(plugin.bin.pvisor, undefined, "The plugin must not shadow the system CLI");
  const nativeBin = path.join(root, "external-runtime", "bin");
  await mkdir(nativeBin, { recursive: true });
  // This executable verifies packaging/routing only. Native file semantics are
  // exercised separately by documentation.native.mjs against the built product.
  const body = `#!${process.execPath}
const args=process.argv.slice(2);
if(args.includes("--help"))console.log("--safe --no-agent-defaults --stage --timeout --overlaynet-deny-all --path --dry-run --json ");
else if(args.includes("--version"))console.log("pvisor external-runtime-fixture");
else console.log(JSON.stringify(args));
`;
  const native = path.join(nativeBin, "pvisor");
  await writeFile(native, body, { mode: 0o755 });
  const config = path.join(root, "npmrc");
  await writeFile(config, "");
  const npmEnv = {
    ...process.env, npm_config_userconfig: config, npm_config_cache: path.join(root, "cache"),
    npm_config_audit: "false", npm_config_fund: "false", npm_config_ignore_scripts: "true",
    npm_config_prefix: prefix,
  };
  delete npmEnv.PVISOR_PLUGIN_BINARY;
  async function packed(directory, metadata) {
    const result = JSON.parse(execFileSync("npm", [
      "pack", directory, "--ignore-scripts", "--json", "--pack-destination", root,
    ], { env: npmEnv, cwd: root, encoding: "utf8", timeout: 30_000 }));
    return { metadata, bytes: await readFile(path.join(root, result[0].filename)) };
  }
  const packages = new Map([
    [plugin.name, await packed(source, plugin)],
  ]);
  const requests = [];
  const server = createServer((request, response) => {
    requests.push(request.url);
    const url = new URL(request.url, `http://${request.headers.host}`);
    const name = decodeURIComponent(url.pathname.split("/")[1]);
    const found = packages.get(name);
    if (!found) { response.writeHead(404); response.end('{"error":"not_found"}'); return; }
    if (url.pathname.endsWith(".tgz")) {
      response.end(found.bytes);
    } else {
      const version = {
        ...found.metadata,
        dist: {
          tarball: `${url.origin}/${name}/-/${name}.tgz`,
          integrity: `sha512-${createHash("sha512").update(found.bytes).digest("base64")}`,
        },
      };
      response.setHeader("content-type", "application/json");
      response.end(JSON.stringify({
        name, "dist-tags": { latest: version.version }, versions: { [version.version]: version },
      }));
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(async () => {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  });
  npmEnv.npm_config_registry = `http://127.0.0.1:${server.address().port}`;
  npmEnv.PATH = `${nativeBin}${path.delimiter}${path.join(prefix, "bin")}${path.delimiter}${process.env.PATH}`;
  const install = await execute("npm", ["install", "--global", `${plugin.name}@${plugin.version}`], {
    cwd: project, env: npmEnv, deadlineMs: 30_000,
  });
  assert.equal(install.exit_code, 0, JSON.stringify(install));
  const version = await execute("sh", ["-eu", "-c", (await documentationCases()).runtime], {
    cwd: project, env: npmEnv,
  });
  assert.equal(version.exit_code, 0, JSON.stringify(version));
  assert.match(version.stdout, /pvisor external-runtime-fixture/);
  const installed = path.join(prefix, "lib/node_modules", plugin.name);
  assert.equal((await lstat(installed)).isSymbolicLink(), false, "Test extracted npm packages, not source links");
  const delivery = await import(pathToFileURL(path.join(installed, "runtime.mjs")));
  assert.equal(await delivery.resolveRuntime(undefined, { searchPath: nativeBin }), native);
  await assert.rejects(delivery.resolveRuntime(undefined, { searchPath: "" }), /Homebrew or pip/);
  assert.ok(requests.some((url) => url.startsWith(`/${plugin.name}`)), "Install through the registry contract, not a source path");
  assert.ok(!requests.some((url) => url.includes("pvisor-agent-runtime")), "npm must never request native packages");
  await assert.rejects(stat(path.join(installed, "test")), { code: "ENOENT" });
  await assert.rejects(stat(path.join(prefix, "bin/pvisor")), { code: "ENOENT" });
  for (const name of ["runtime.json", "release.mjs", "pvisor.mjs"]) {
    await assert.rejects(stat(path.join(installed, name)), { code: "ENOENT" });
  }
  const argv = ["apply", "/stage with spaces", "--path", "src", "--dry-run"];
  const forwarded = await execute(native, argv, { cwd: project, env: npmEnv });
  assert.equal(forwarded.exit_code, 0, JSON.stringify(forwarded));
  assert.deepEqual(JSON.parse(forwarded.stdout), argv);
  const client = await mcpClient(project, npmEnv, path.join(installed, "mcp.mjs"));
  try {
    const doctor = await client.call("pvisor_doctor");
    assert.equal(doctor.project, project);
    assert.equal(doctor.binary, native);
    assert.match(doctor.stdout, /external-runtime-fixture/);
  } finally {
    await client.close();
  }
  const retained = path.join(root, "retained-stage.txt");
  await writeFile(retained, "pending candidate");
  const uninstall = await execute("npm", ["uninstall", "--global", plugin.name], { cwd: project, env: npmEnv });
  assert.equal(uninstall.exit_code, 0, JSON.stringify(uninstall));
  await assert.rejects(stat(installed), { code: "ENOENT" });
  assert.equal(await readFile(native, "utf8"), body);
  assert.equal(await readFile(retained, "utf8"), "pending candidate");
});
