import assert from "node:assert/strict";
import registerPi from "../pi.mjs";
import { registerDsh } from "../dsh.mjs";

/** A manually released approval callback: no timing-dependent polling or sleeps. */
export function approvalGate() {
  const entered = Promise.withResolvers();
  const answer = Promise.withResolvers();
  return {
    entered: entered.promise,
    release: answer.resolve,
    fail: answer.reject,
    async request(request) {
      entered.resolve(request);
      return answer.promise;
    },
  };
}

/** Observe settlement immediately, and never wait forever for an unentered gate. */
export function approvalOperation(host, action, input, signal, timeoutMs = 5000) {
  const gate = host.gate;
  const pending = host.call(action, input);
  let done = false;
  const settled = pending.then(
    () => { done = true; return null; },
    (error) => { done = true; return error; },
  );
  const interrupted = Promise.withResolvers();
  const cancel = (reason) => {
    if (done) return;
    host.controller.abort(reason);
    gate.release(false);
    interrupted.reject(reason);
  };
  // This promise also needs a handler when callers only await result/close.
  interrupted.promise.catch(() => {});
  const onAbort = () => cancel(signal.reason);
  signal?.addEventListener("abort", onAbort, { once: true });
  const timer = setTimeout(() => cancel(new Error("Approval fixture deadline exceeded")), timeoutMs);
  if (signal?.aborted) onAbort();
  return {
    result: pending,
    settled,
    entered: () => Promise.race([
      gate.entered,
      settled.then((error) => { throw error || new Error("Operation settled before approval"); }),
      interrupted.promise,
    ]),
    async close() {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      gate.release(false);
      if (!done) host.controller.abort(new Error("Approval fixture closing"));
      await settled;
    },
  };
}

/**
 * Mock host registration and approval only, not native filesystem semantics.
 * DSH dispatch models its documented detached, recursively frozen JSON args.
 * This is not an execution of the installed host compiler/registry or its UI.
 */
export function approvalHost(kind, project, makeBackend) {
  const definitions = new Map();
  const commands = new Map();
  const controller = new AbortController();
  const notices = [];
  const agent = { session: { header: { cwd: project } } };
  const backend = (cwd) => {
    const client = makeBackend(cwd);
    return {
      project: () => client.project(),
      operate: (action, input, signal) => client.operate(
        action, input, signal ? AbortSignal.any([controller.signal, signal]) : controller.signal,
      ),
    };
  };
  const context = {
    cwd: project, hasUI: true,
    ui: {
      confirm: (title, message) => host.gate.request({ title, reason: message }),
      notify: (message, level) => notices.push({ message, level }),
    },
  };
  const host = {
    gate: approvalGate(),
    controller,
    notices,
    moveProject(next) {
      context.cwd = next;
      agent.session.header.cwd = next;
    },
    async call(action, raw) {
      if (kind === "pi") {
        const previous = notices.length;
        await commands.get("pvisor").handler(
          action === "apply" ? `apply ${JSON.stringify(raw)}` : `${action} ${raw.selector}`,
          context,
        );
        const notice = notices.slice(previous).at(-1);
        if (notice?.level === "error") throw new Error(notice.message);
        return;
      }
      const args = structuredClone(raw);
      if (args.paths) Object.freeze(args.paths);
      Object.freeze(args);
      return definitions.get(`pvisor_${action}`).execute(args, {
        agent, callId: "approval-race", signal: controller.signal,
      });
    },
    approve() { host.gate.release(kind === "pi" ? true : "allowed-once"); },
    reject(outcome = "rejected") { host.gate.release(kind === "pi" ? false : outcome); },
  };
  if (kind === "pi") {
    registerPi({
      registerTool() {},
      registerCommand: (name, command) => commands.set(name, command),
      on() {},
    }, { createBackend: backend });
  } else {
    assert.equal(kind, "dsh");
    registerDsh({
      tools: { register: (tool) => definitions.set(tool.name, tool) },
      approval: { request: (request) => host.gate.request(request) },
    }, (definition) => definition, backend);
  }
  return host;
}
