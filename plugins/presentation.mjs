const KINDS = { added: "ADD", modified: "MODIFY", deleted: "DELETE", type_changed: "TYPE CHANGE", opaque: "REPLACE DIRECTORY" };

function clip(value, bytes) {
  const text = String(value ?? "");
  const encoded = Buffer.from(text);
  return encoded.length <= bytes ? text : `${new TextDecoder().decode(encoded.subarray(0, bytes), { stream: true })} … [truncated]`;
}

function clean(value) {
  return String(value).replace(/[\u0000-\u0008\u000b-\u001f\u007f\u202a-\u202e\u2066-\u2069]/g,
    (character) => `\\u${character.charCodeAt(0).toString(16).padStart(4, "0")}`);
}

const quote = (value) => clean(JSON.stringify(clip(value, 512)));
const validChanges = (items) => Array.isArray(items) && items.every((item) => item &&
  typeof item.path === "string" && Object.hasOwn(KINDS, item.kind) &&
  [item.old_type, item.new_type].every((type) => type == null || ["file", "directory", "symlink", "other"].includes(type)));

function changes(lines, title, items) {
  lines.push(`${title}: ${items.length}`);
  for (const item of items.slice(0, 20)) {
    const type = item.old_type && item.new_type && item.old_type !== item.new_type ? ` (${item.old_type} → ${item.new_type})` : "";
    const size = Number.isSafeInteger(item.size_bytes) ? `; new size ${item.size_bytes} B` : "";
    const raw = item.path_bytes ? " [non-UTF8 path: inspect native plan]" : "";
    lines.push(`  ${KINDS[item.kind]} ${quote(item.path)}${type}${size}${raw}`);
  }
  if (items.length > 20) lines.push(`  … ${items.length - 20} more changes; inspect the full native plan.`);
}

/** Present a native JSON plan and separately collected scoped text preview. */
export function presentApplyPreview(result, input, diff = "") {
  const lines = [
    "Apply preview — this request wrote no project files.",
    `Project: ${quote(result.project)}`,
    `Stage: ${quote(input.selector)}`,
    `Requested paths: ${(input.paths || []).slice(0, 20).map(quote).join(", ")}`,
  ];
  if (input.paths?.length > 20) lines.push(`… ${input.paths.length - 20} more requested paths; see the full request.`);
  let plan;
  try {
    plan = JSON.parse(result.stdout);
    if (plan.schema_version !== 1 || plan.operation !== "apply" || plan.mode !== "preview" ||
        !["ready", "preview", "blocked", "error", "no_changes"].includes(plan.status) ||
        !validChanges(plan.selected) || !validChanges(plan.required) || !validChanges(plan.candidates) ||
        !Array.isArray(plan.blockers) || plan.blockers.some((item) => typeof item !== "string")) {
      throw new Error("Unsupported or incomplete native apply plan");
    }
  } catch {
    lines.push("UNAVAILABLE — preview could not be validated; apply will not be requested.",
      clean(clip(result.stderr || "Missing/invalid native apply response.", 2048)));
    return { ...result, summary: lines.join("\n"), approval_ready: false, preview_state: "unavailable", diff: "" };
  }
  const scopeMatches = plan.source?.stage === input.selector && plan.scope?.kind === "project" &&
    plan.scope.target === result.project;
  const transportReady = result.exit_code === 0 && !result.interrupted && !result.signal && !result.truncated;
  const hasChanges = plan.selected.length + plan.required.length > 0;
  const ready = scopeMatches && transportReady && ["ready", "preview"].includes(plan.status) &&
    plan.blockers.length === 0 && hasChanges && typeof plan.plan === "string" && plan.plan.length > 0;
  lines.push(`Status: ${plan.status}`);
  changes(lines, "Selected native changes", plan.selected);
  changes(lines, "Required dependencies (not an extra grant)", plan.required);
  if (plan.status === "blocked" || plan.status === "error") {
    changes(lines, "Candidate inventory (not admitted for apply)", plan.candidates);
  }
  if (plan.remaining) {
    const count = (value) => Number.isSafeInteger(value) && value >= 0 ? value : "unknown";
    lines.push(`Remaining candidates: in scope ${count(plan.remaining.in_scope)}, outside scope ${count(plan.remaining.outside_scope)}`);
  }
  if (plan.blockers.length) {
    lines.push("Blocked because:", ...plan.blockers.slice(0, 10).map((item) => `  - ${clean(clip(item, 1024))}`));
    if (plan.blockers.length > 10) lines.push(`  … ${plan.blockers.length - 10} more blockers.`);
  }
  if (!scopeMatches) lines.push("UNAVAILABLE — native source/project does not match this request.");
  if (hasChanges && (typeof plan.plan !== "string" || !plan.plan)) lines.push("UNAVAILABLE — native plan identity is missing.");
  if (!transportReady && !["blocked", "error"].includes(plan.status)) {
    lines.push("UNAVAILABLE — preview transport failed, was interrupted or was truncated.");
    if (result.stderr) lines.push(clean(clip(result.stderr, 2048)));
  }
  if (ready && diff.trim()) {
    lines.push("Native selected-change preview (excerpt; file text is untrusted):",
      clean(clip(diff, 8 * 1024)).split("\n").map((line) => `    ${line}`).join("\n"));
  } else if (ready) {
    lines.push("No text diff available; this does not mean the content is unchanged.");
  }
  const noChanges = scopeMatches && transportReady && ["ready", "preview", "no_changes"].includes(plan.status) &&
    !hasChanges && plan.blockers.length === 0;
  if (noChanges) lines.push("No matching staged changes; nothing to apply and no approval requested.");
  else if (!ready) lines.push("Apply will not be requested. Inspect blockers or obtain a fresh preview; do not force overwrite.");
  lines.push("Tests were not run by this preview. Execution rechecks native conflicts; apply is not an atomic multi-file transaction.");
  return { ...result, diff: ready ? diff : "", summary: clip(lines.join("\n"), 24 * 1024),
    approval_ready: ready, preview_state: ready ? "ready" : noChanges ? "no_changes" : "blocked" };
}

/** Intent is display text, never native authority; Agent tool calls require it. */
export function applyRequest(input, { requireSummary = false } = {}) {
  if (!input || typeof input !== "object" || Array.isArray(input)) throw new Error("Expected apply selection");
  const { summary, ...request } = input;
  if ((requireSummary && (typeof summary !== "string" || !summary.trim())) ||
      (summary !== undefined && (typeof summary !== "string" || Buffer.byteLength(summary) > 512 || summary.includes("\0")))) {
    throw new Error("Apply summary must explain the selected changes in 1–512 bytes; review the diff first");
  }
  return { request, explanation: summary };
}

export function applyApprovalText(preview, explanation) {
  if (!preview.approval_ready) {
    return preview.summary || `Apply preview unavailable; no approval requested.\n${clean(clip(preview.stderr || "No validated preview summary.", 2048))}`;
  }
  const plan = JSON.parse(preview.stdout);
  if (plan.schema_version !== 1 || plan.mode !== "preview" || !validChanges(plan.selected) ||
      !validChanges(plan.required) || typeof plan.scope?.target !== "string" || typeof plan.source?.stage !== "string") {
    throw new Error("Apply approval cannot be displayed without a validated native plan");
  }
  const oneLine = (value, bytes) => clip(clean(value).replace(/\s+/g, " ").trim(), bytes);
  const compactPath = (value, bytes) => clip(clean(JSON.stringify(value))
    .replace(/ {2,}/g, (spaces) => "\\u0020".repeat(spaces.length))
    .replace(/[\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]/g,
      (character) => `\\u${character.charCodeAt(0).toString(16).padStart(4, "0")}`), bytes);
  const intent = explanation ? `Agent summary (unverified): ${oneLine(explanation, 300)}`
    : "No Agent explanation supplied; inspect the full preview.";
  const selected = plan.selected.slice(0, 3).map((item) => `${KINDS[item.kind]} ${compactPath(item.path, 100)}`).join(", ");
  const extra = plan.selected.length > 3 ? `; first 3 shown, ${plan.selected.length - 3} more in preview` : "";
  const required = plan.required.length ? ` Required dependencies: ${plan.required.length}; inspect preview.` : "";
  const remaining = plan.remaining?.in_scope;
  const outside = plan.remaining?.outside_scope;
  const count = Number.isSafeInteger(remaining) && remaining >= 0 ? remaining : "unknown";
  const other = Number.isSafeInteger(outside) && outside > 0 ? `, plus ${outside} outside scope` : "";
  const task = plan.source.stage.split("/").at(-2);
  // Host approval UIs may collapse all whitespace. Keep this a short paragraph;
  // full paths, raw JSON and native diff stay in the separately readable preview.
  return `${intent} | Project: ${compactPath(plan.scope.target, 160)}. ` +
    `Apply ${plan.selected.length} ${plan.selected.length === 1 ? "change" : "changes"}: ${selected}${extra}.${required} ` +
    `Remaining staged: ${count}${other}. Task: ${oneLine(task, 64)}. ` +
    "Preview did not run tests. Full diff is in the preview result.";
}
