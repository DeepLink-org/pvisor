// Presentation only: native planning and execution retain all authority.
const ACTIONS = { added: "Add", modified: "Change", deleted: "Delete", type_changed: "Change type", opaque: "Opaque directory change" };
const TYPES = new Set(["file", "directory", "symlink", "other"]);
const LIMIT = 12;
// C0/C1, line separators, and every Unicode Bidi_Control code point.
// Keep one rule for visible labels and suppression of runnable commands.
const CONTROLS = /[\u0000-\u001f\u007f-\u009f\u061c\u200e\u200f\u2028-\u202e\u2066-\u2069]/;

/** Quote untrusted labels on one line, including terminal/bidi controls. */
export function label(value, limit = 240) {
  const text = JSON.stringify(String(value)).replace(new RegExp(CONTROLS.source, "g"),
    (char) => `\\u${char.charCodeAt(0).toString(16).padStart(4, "0")}`);
  return text.length <= limit ? text : `${text.slice(0, limit)}… [label truncated]`;
}

const count = (value) => Number.isSafeInteger(value) && value >= 0;
const identity = (entry) => JSON.stringify(entry.path_bytes ?? entry.path);
const changes = (value) => Array.isArray(value) && value.every((entry) =>
  entry && typeof entry.path === "string" && typeof entry.kind === "string" && Object.hasOwn(ACTIONS, entry.kind) &&
  [entry.old_type, entry.new_type].every((type) => type == null || TYPES.has(type)) &&
  (entry.size_bytes == null || count(entry.size_bytes)) &&
  (entry.mode == null || count(entry.mode)) &&
  (entry.path_bytes == null || (Array.isArray(entry.path_bytes) &&
    entry.path_bytes.length > 0 && entry.path_bytes.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255))));

/** Normalize only a complete native schema-1 preview; never parse diff prose. */
export function summarizePreview(result, input, project) {
  let native;
  let issue;
  if (result.truncated) issue = "Native output was truncated; complete change information is unavailable.";
  else if (result.interrupted || result.signal) issue = "Native preview was interrupted; outcome is unknown.";
  else {
    try { native = JSON.parse(result.stdout); }
    catch { issue = "Native preview JSON is missing or malformed."; }
  }
  if (!issue && !(native?.schema_version === 1 && native.operation === "apply" &&
      native.mode === "preview" && ["ready", "no_changes", "blocked", "error"].includes(native.status) &&
      native.source?.stage === input.selector && native.scope?.kind === "project" &&
      (native.scope.target === project || (native.scope.target == null && native.status === "blocked")) &&
      changes(native.selected) && changes(native.required) && changes(native.candidates) &&
      Array.isArray(native.blockers) && native.blockers.every((item) => typeof item === "string") &&
      native.remaining && [native.remaining.in_scope, native.remaining.outside_scope].every((n) => n === null || count(n)))) {
    issue = "Native preview schema, identity or project scope is unsupported or incomplete.";
  }
  const base = { project, selector: input.selector, paths: [...input.paths] };
  if (issue) return { ...base, can_confirm: false, status: "unknown", issue, diagnostic: result.stderr };
  const planned = result.exit_code === 0 && ["ready", "no_changes"].includes(native.status) && native.blockers.length === 0;
  const included = new Set([...native.selected, ...native.required].map(identity));
  return {
    ...base,
    can_confirm: planned,
    status: native.status,
    selected: planned ? native.selected : null,
    required: planned ? native.required : null,
    // On a blocked plan an empty selected array does not mean nothing matched.
    pending: planned ? native.candidates.filter((entry) => !included.has(identity(entry))) : null,
    candidates: native.candidates.length,
    unplanned_candidates: planned ? [] : native.candidates,
    remaining: native.remaining,
    blockers: native.blockers,
    diagnostic: result.stderr,
  };
}

function list(title, entries, render) {
  if (entries === null) return [`${title}: unknown (native plan not ready)`];
  return [
    `${title}: ${entries.length}`,
    ...entries.slice(0, LIMIT).map((entry) => `  ${render(entry)}`),
    ...(entries.length > LIMIT ? [`  … ${entries.length - LIMIT} more entries not shown`] : []),
  ];
}

function change(entry) {
  const type = entry.old_type && entry.new_type && entry.old_type !== entry.new_type
    ? `${entry.old_type} → ${entry.new_type}` : entry.new_type ?? entry.old_type ?? "type unknown";
  const details = [type];
  if (entry.size_bytes != null) details.push(`${entry.size_bytes} bytes (reported size)`);
  if (entry.mode != null) details.push(`mode ${entry.mode.toString(8)} (reported, not a mode delta)`);
  if (entry.path_bytes) details.push("non-UTF-8 identity: display path is lossy; inspect native details");
  return `${ACTIONS[entry.kind]} ${label(entry.path)} — ${details.join("; ")}`;
}

const shell = (value) => `'${value.replaceAll("'", "'\\''")}'`;

/** Shared bounded Pi/DSH decision text. Raw preview stdout remains on the result. */
export function formatPreview(summary) {
  const lines = [
    "pVisor apply — change summary",
    `Project: ${label(summary.project)}`,
    `Stage: ${label(summary.selector)}`,
    ...list("Requested paths (directory selections include descendants)", summary.paths, (item) => label(item)),
    `Native preview: ${summary.status}`,
  ];
  if (summary.diagnostic) lines.push(`Native diagnostic: ${label(summary.diagnostic, 1000)}`);
  if (summary.issue) lines.push(summary.issue);
  else {
    lines.push(
      ...list("Selected changes", summary.selected, change),
      ...list("Additional required changes (also applied)", summary.required, change),
      ...list("Other staged candidates (NOT applied by this selection)", summary.pending, change),
      `Native candidate inventory: ${summary.candidates} entries (not a count of selected files).`,
      ...(summary.unplanned_candidates.length ? list("Observed candidates (NOT an approved or ready plan)", summary.unplanned_candidates, change) : []),
      `Remaining after proposed apply: in project ${summary.remaining.in_scope ?? "unknown"}; outside scope ${summary.remaining.outside_scope ?? "unknown"}.`,
      ...list("Blockers / conflicts", summary.blockers, (item) => label(item, 500)),
    );
    if (summary.selected?.length === 0 && summary.required?.length === 0) lines.push("No matching staged changes; this is a no-op, not an all-pending apply.");
  }
  lines.push(
    "Content/binary classification, previous modes, semantic purpose and test results are not provided by this preview. A modified entry can be content or metadata-only; no behavior or test success is inferred.",
    "Review details before deciding. Native execution rechecks conflicts; this preview is not a reservation, approval or execution evidence.",
  );
  const commands = [
    `pvisor review ${shell(summary.selector)} --diff`,
    `pvisor apply ${shell(summary.selector)} ${summary.paths.map((item) => `--path ${shell(item)}`).join(" ")} --dry-run --diff`,
  ];
  // Never show a partial runnable command or embed raw control characters.
  if (commands.join("\n").length <= 3000 && !commands.some((command) => CONTROLS.test(command))) {
    lines.push("Detailed native review (run in the named project):", ...commands);
  } else lines.push("Review commands omitted (long or control-containing paths). Use the exact selector and paths from the tool input with native review --diff and apply --dry-run --diff; never copy truncated display labels.");
  lines.push(summary.can_confirm ? "Publish only the selected and additional required changes above?" : "Apply unavailable: inspect the native preview/error and resolve unknowns or blockers; no confirmation requested.");
  return lines.join("\n");
}

