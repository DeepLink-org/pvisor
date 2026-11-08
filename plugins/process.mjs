import { spawn } from "node:child_process";

const OUTPUT_LIMIT = 256 * 1024;

/**
 * Drain both streams after reaching the display budget. Cancellation requests
 * native teardown; forced CLI termination remains an uncertain native outcome.
 * This supervises runtime probes as well as Session commands.
 */
export function execute(binary, args, { cwd, env, signal, deadlineMs = 30_000 } = {}) {
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const child = spawn(binary, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"], shell: false });
    const buffers = { stdout: [], stderr: [] };
    const sizes = { stdout: 0, stderr: 0 };
    let truncated = false;
    let interrupted = false;
    let escalation;
    let drainDeadline;
    const interrupt = () => {
      if (interrupted) return;
      interrupted = true;
      child.kill("SIGTERM");
      escalation = setTimeout(() => {
        child.kill("SIGKILL");
        // A lost supervisor's descendants may retain the output descriptors.
        // Stop waiting for those streams without claiming native teardown.
        drainDeadline = setTimeout(() => {
          child.stdout.destroy();
          child.stderr.destroy();
        }, 5_000);
        drainDeadline.unref();
      }, 5_000);
      escalation.unref();
    };
    const deadline = setTimeout(interrupt, deadlineMs);
    deadline.unref();
    const cleanup = () => {
      clearTimeout(deadline);
      clearTimeout(escalation);
      clearTimeout(drainDeadline);
      signal?.removeEventListener("abort", interrupt);
    };
    signal?.addEventListener("abort", interrupt, { once: true });
    if (signal?.aborted) interrupt();
    for (const stream of ["stdout", "stderr"]) {
      child[stream].on("data", (bytes) => {
        const remaining = OUTPUT_LIMIT - sizes[stream];
        if (bytes.length > remaining) truncated = true;
        if (remaining > 0) {
          buffers[stream].push(bytes.subarray(0, remaining));
          sizes[stream] += Math.min(remaining, bytes.length);
        }
      });
    }
    child.on("error", (error) => { cleanup(); reject(error); });
    child.on("close", (code, terminationSignal) => {
      cleanup();
      resolve({
        exit_code: code,
        signal: terminationSignal,
        interrupted,
        stdout: Buffer.concat(buffers.stdout).toString("utf8"),
        stderr: Buffer.concat(buffers.stderr).toString("utf8"),
        truncated,
      });
    });
  });
}
