import assert from "node:assert/strict";
import registerPi from "../pi.mjs";
import { registerDsh } from "../dsh.mjs";

/** Observe early failures as well as gate entry, and always settle on close. */
export function approvalOperation(host, input, run = (callback) => Promise.resolve().then(callback)) {
  const settled = run(() => host.apply(input));
  settled.catch(() => {});
  const ready = Promise.race([
    host.ready,
    settled.then(() => assert.fail("apply settled before approval")),
  ]);
  ready.catch(() => {});
  return {
    settled,
    ready,
    async close() {
      host.reject();
      await settled.catch(() => {});
    },
  };
}

/** Hold the actual adapter at its public host approval callback. */
export function pendingApproval(kind, project, makeBackend) {
  let entered;
  let answer;
  const ready = new Promise((resolve) => { entered = resolve; });
  const decision = new Promise((resolve) => { answer = resolve; });
  const definitions = new Map();
  const notices = [];
  const requests = [];
  let command;
  const approve = async (request) => {
    requests.push(request);
    entered(request);
    return decision;
  };
  if (kind === "pi") {
    registerPi({
      registerTool() {},
      registerCommand: (_name, definition) => { command = definition; },
      on() {},
    }, { createBackend: makeBackend });
  } else {
    assert.equal(kind, "dsh");
    registerDsh({
      tools: { register: (definition) => definitions.set(definition.name, definition) },
      approval: { request: approve },
    }, (definition) => definition, makeBackend);
  }
  return {
    ready,
    requests,
    release: () => answer(kind === "pi" ? true : "allowed-once"),
    reject: () => answer(kind === "pi" ? false : "rejected"),
    async apply(input) {
      if (kind === "dsh") {
        return definitions.get("pvisor_apply").execute(input, {
          agent: { session: { header: { cwd: project } } },
          callId: "concurrent-approval",
          signal: AbortSignal.timeout(10_000),
        });
      }
      await command.handler(`apply ${JSON.stringify(input)}`, {
        cwd: project,
        hasUI: true,
        ui: {
          confirm: (title, message) => approve({ title, message }),
          notify: (message, level) => notices.push({ message, level }),
        },
      });
      const notice = notices.at(-1);
      if (notice?.level === "error") throw new Error(notice.message);
      return JSON.parse(notice.message);
    },
  };
}
