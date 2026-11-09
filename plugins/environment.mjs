import { access, realpath, stat } from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";

function within(parent, child) {
  const relative = path.relative(parent, child);
  return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative));
}

/** An operator-selected installed toolchain, never a model-supplied share or rustup shim. */
export async function rustEnvironment(requested, project, state) {
  if (requested === undefined) return undefined;
  if (typeof requested !== "string" || !path.isAbsolute(requested) || requested.includes("\0") || requested.includes(":")) {
    throw new Error("PVISOR_PLUGIN_RUST_TOOLCHAIN must be an absolute toolchain directory");
  }
  const root = await realpath(requested);
  if (!(await stat(root)).isDirectory() || root === "/" || root.includes(":") ||
      within(project, root) || within(root, project) || within(state, root) || within(root, state) ||
      ["/tmp", "/var/tmp", "/private/tmp", "/private/var/tmp"].some((temporary) => within(temporary, root))) {
    throw new Error("Rust toolchain must be outside the project, plugin state and private temporary paths");
  }
  const binaries = new Set();
  for (const name of ["cargo", "rustc", "rustdoc"]) {
    const binary = path.join(root, "bin", name);
    const resolved = await realpath(binary);
    const info = await stat(binary);
    const identity = `${info.dev}:${info.ino}`;
    // rustup shims can symlink or hardlink all three commands to one proxy.
    if (!within(root, resolved) || !info.isFile() || path.basename(resolved) === "rustup" || binaries.has(identity)) {
      throw new Error(`Rust profile requires a real toolchain binary: ${binary}`);
    }
    binaries.add(identity);
    await access(binary, constants.X_OK);
  }
  return { root, bin: path.join(root, "bin") };
}

/** Private, offline build state; no host Cargo credentials, config or registry cache is shared. */
export function rustTaskEnvironment(profile, home) {
  if (!profile) return {};
  return {
    PATH: `${profile.bin}${path.delimiter}/usr/local/bin${path.delimiter}/usr/bin${path.delimiter}/bin`,
    CARGO_HOME: path.join(home, ".cargo"),
    CARGO_TARGET_DIR: path.join(home, "target"),
    CARGO_NET_OFFLINE: "true",
    RUSTC: path.join(profile.bin, "rustc"),
    RUSTDOC: path.join(profile.bin, "rustdoc"),
  };
}
