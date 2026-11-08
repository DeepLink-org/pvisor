import { realpath, stat } from "node:fs/promises";
import path from "node:path";

function overlaps(a, b) {
  const inside = (parent, child) => {
    const relative = path.relative(parent, child);
    return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative));
  };
  return inside(a, b) || inside(b, a);
}

/** Operator policy only. No tool schema accepts these values. */
export function executionPolicy(mode = "auto", allowHostFallback = "false", rootfs) {
  if (!["auto", "vm", "host"].includes(mode)) throw new Error("PVISOR_PLUGIN_EXECUTION must be auto, vm or host");
  if (!["true", "false"].includes(allowHostFallback)) throw new Error("PVISOR_PLUGIN_ALLOW_HOST_FALLBACK must be true or false");
  if (rootfs !== undefined && (typeof rootfs !== "string" || !path.isAbsolute(rootfs) || rootfs.includes("\0"))) {
    throw new Error("PVISOR_PLUGIN_VM_ROOTFS must be an absolute prepared Linux rootfs directory");
  }
  return { mode, allowHostFallback: allowHostFallback === "true", rootfs };
}

/** Preflight selects once, before user argv starts. Never retry after launch. */
export async function selectExecution(policy, { binary, project, state, execute, signal, hostToolchain = false }) {
  if (policy.mode === "host") {
    return { requested: "host", selected: "host", ready: true, fallback: false };
  }
  let rootfs;
  if (policy.rootfs !== undefined) {
    rootfs = await realpath(policy.rootfs);
    const info = await stat(rootfs);
    if (!info.isDirectory() || (info.mode & 0o022) !== 0 ||
        (process.getuid && ![0, process.getuid()].includes(info.uid)) ||
        [project, state].some((protectedPath) => overlaps(rootfs, protectedPath))) {
      throw new Error("VM rootfs must be an owned/non-writable-by-others directory disjoint from project and plugin state; host / is forbidden");
    }
  }
  const observed = await execute(binary, ["vm-check", "--json"], {
    cwd: project, signal, deadlineMs: 10_000, outputLimit: 256 * 1024,
  });
  if (observed.exit_code !== 0 || observed.interrupted || observed.truncated) {
    throw new Error("VM preflight unavailable; upgrade the native runtime for vm-check. No automatic downgrade or task launch occurred; explicitly select host only if intended");
  }
  let capability;
  try { capability = JSON.parse(observed.stdout); } catch { throw new Error("Invalid native VM preflight; no downgrade allowed"); }
  if (capability.schema_version !== 1 || capability.operation !== "vm-check" ||
      typeof capability.can_create_vm !== "boolean" || typeof capability.reason !== "string" ||
      typeof capability.backend !== "string" || capability.boot_verified !== false ||
      !["embedded", "bundled_unloaded", "missing", "unsupported"].includes(capability.firmware)) {
    throw new Error("Invalid native VM preflight contract; no downgrade allowed");
  }
  const reasons = [];
  if (!capability.can_create_vm) reasons.push(capability.reason);
  if (["missing", "unsupported"].includes(capability.firmware)) reasons.push("VM firmware unavailable in this native installation");
  if (!rootfs) reasons.push("Set PVISOR_PLUGIN_VM_ROOTFS to a prepared Linux rootfs; the host / is never selected implicitly");
  const ready = reasons.length === 0;
  if (ready && hostToolchain) {
    throw new Error("Host Rust toolchain shares are not a VM toolchain; prepare tools inside the VM rootfs and unset PVISOR_PLUGIN_RUST_TOOLCHAIN");
  }
  const fallback = !ready && policy.mode === "auto" && policy.allowHostFallback;
  return { requested: policy.mode, selected: ready ? "vm" : fallback ? "host" : null,
    ready: ready || fallback, fallback, reason: reasons.join("; ") || null,
    rootfs: rootfs ?? null, virtualization: capability,
    workflow: "preflight only; guest boot, tools, staging and completion are verified by the actual run, not this check" };
}
