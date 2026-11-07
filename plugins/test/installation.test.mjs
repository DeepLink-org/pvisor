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
import { runtime } from "../runtime.mjs";
import { documentationCases } from "./documentation.mjs";
import { mcpClient } from "./mcp-client.mjs";

test("DOC-PLUGINS-07: documented npm install selects a native dependency without a source path or lifecycle scripts", { timeout: 60_000 }, async (t) => {
  const root = await realpath(await mkdtemp(path.join(tmpdir(), "plugin-install-")));
  t.after(() => rm(root, { recursive: true, force: true }));
  const project = path.join(root, "project");
  const prefix = path.join(root, "prefix");
  await mkdir(project);
  const target = runtime.platforms[`${process.platform}-${process.arch}`];
  assert.ok(target, "Run installation contracts on supported npm targets");
  const source = fileURLToPath(new URL("../", import.meta.url));
  const plugin = JSON.parse(await readFile(path.join(source, "package.json"), "utf8"));
  const nativeRoot = path.join(root, "native-fixture");
  await mkdir(path.join(nativeRoot, "bin"), { recursive: true });
  const native = {
    name: target.package, version: plugin.version, os: [process.platform], cpu: [process.arch],
    files: ["bin"], exports: { "./package.json": "./package.json" },
    pvisorRuntimeVersion: runtime.version, pvisorRuntimeCommit: "a".repeat(40),
    pvisorRuntimeDistribution: `${runtime.version}+g1.aaaaaaa`,
  };
  await writeFile(path.join(nativeRoot, "package.json"), JSON.stringify(native));
  // This executable verifies packaging/routing only. Native file semantics are
  // exercised separately by documentation.native.mjs against the built product.
  const body = `#!${process.execPath}
const args=process.argv.slice(2);
if(args.includes("--help"))console.log("--safe --no-agent-defaults --stage --timeout --overlaynet-deny-all --path --dry-run --json ");
else if(args.includes("--version"))console.log("pvisor npm-install-fixture");
else console.log(JSON.stringify(args));
`;
  for (const name of ["pvisor", "pvisor-replay", "pvisor-daemon"]) {
    await writeFile(path.join(nativeRoot, "bin", name), body, { mode: 0o755 });
  }
  await writeFile(path.join(nativeRoot, "bin/libkrunfw.SOURCE"), "fixture metadata");
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
    [native.name, await packed(nativeRoot, native)],
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
  npmEnv.PATH = `${path.join(prefix, "bin")}${path.delimiter}${process.env.PATH}`;
  const install = await execute("sh", ["-eu", "-c", (await documentationCases()).install], {
    cwd: project, env: npmEnv, deadlineMs: 30_000,
  });
  assert.equal(install.exit_code, 0, JSON.stringify(install));
  assert.match(install.stdout, /pvisor npm-install-fixture/);
  const installed = path.join(prefix, "lib/node_modules", plugin.name);
  assert.equal((await lstat(installed)).isSymbolicLink(), false, "Test extracted npm packages, not source links");
  const delivery = await import(pathToFileURL(path.join(installed, "runtime.mjs")));
  const deliveredRuntime = path.dirname(path.dirname(await delivery.resolveRuntime()));
  const deliveredMetadata = JSON.parse(await readFile(path.join(deliveredRuntime, "package.json"), "utf8"));
  assert.equal(deliveredMetadata.name, native.name, "npm must install the matching native optional dependency");
  assert.equal((await lstat(deliveredRuntime)).isSymbolicLink(), false);
  assert.ok(requests.some((url) => url.startsWith(`/${plugin.name}`)), "Install through the registry contract, not a source path");
  await assert.rejects(stat(path.join(installed, "test")), { code: "ENOENT" });
  const argv = ["apply", "/stage with spaces", "--path", "src", "--dry-run"];
  const forwarded = await execute(path.join(prefix, "bin/pvisor"), argv, { cwd: project, env: npmEnv });
  assert.equal(forwarded.exit_code, 0, JSON.stringify(forwarded));
  assert.deepEqual(JSON.parse(forwarded.stdout), argv);
  const client = await mcpClient(project, npmEnv, path.join(installed, "mcp.mjs"));
  try {
    const doctor = await client.call("pvisor_doctor");
    assert.equal(doctor.project, project);
    assert.match(doctor.stdout, /npm-install-fixture/);
  } finally {
    await client.close();
  }
  const retained = path.join(root, "retained-stage.txt");
  await writeFile(retained, "pending candidate");
  const uninstall = await execute("npm", ["uninstall", "--global", plugin.name], { cwd: project, env: npmEnv });
  assert.equal(uninstall.exit_code, 0, JSON.stringify(uninstall));
  await assert.rejects(stat(installed), { code: "ENOENT" });
  assert.equal(await readFile(retained, "utf8"), "pending candidate");
});
