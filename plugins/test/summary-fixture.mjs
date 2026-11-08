/** Native schema-1 shape, not execution evidence or filesystem emulation. */
export function nativePreview(input, project = "/project", overrides = {}) {
  const selected = input.paths.map((path) => ({ path, kind: "modified", old_type: "file", new_type: "file" }));
  return {
    exit_code: 0, interrupted: false, signal: null, truncated: false, stderr: "",
    stdout: JSON.stringify({
      schema_version: 1, operation: "apply", mode: "preview", status: "ready",
      source: { session_id: "fixture", stage: input.selector, execution_id: null, generation: 1 },
      scope: { kind: "project", target: project },
      selected, required: [], candidates: selected, blockers: [],
      remaining: { in_scope: 0, outside_scope: 0 }, execution: null,
      plan: "fixture-plan", picker: { excluded: [] },
      ...overrides,
    }),
  };
}
