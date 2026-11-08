/** Decode the public native review contract; never infer scope from path spelling. */
export function structuredReview(result, project) {
  const unavailable = (reason) => ({
    review_status: "unavailable", changes: null, counts: null,
    in_scope: null, outside_scope: null, unclassified: null, review_error: reason,
  });
  if (result.exit_code !== 0 || result.interrupted || result.truncated) {
    return unavailable("Native JSON review failed, was interrupted or exceeded its read budget.");
  }
  try {
    const bundle = JSON.parse(result.stdout);
    if (bundle.state === "unknown") return unavailable(bundle.reason || "Native review outcome is unknown.");
    const handoff = bundle.handoff;
    const context = bundle.review_context;
    const changes = bundle.filesystem?.changes;
    const types = ["file", "directory", "symlink", "other"];
    const validEntry = (entry) => entry && typeof entry.path === "string" &&
      ["added", "modified", "deleted", "type_changed", "opaque"].includes(entry.kind) &&
      [entry.old_type, entry.new_type].every((type) => type == null || types.includes(type)) &&
      (entry.size_bytes == null || (Number.isSafeInteger(entry.size_bytes) && entry.size_bytes >= 0)) &&
      (entry.path_bytes == null || (Array.isArray(entry.path_bytes) &&
        entry.path_bytes.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)));
    if (bundle.schema_version !== 6 || bundle.filesystem?.target !== project ||
        handoff?.schema_version !== 1 || handoff.project_root !== project ||
        typeof handoff.classification_available !== "boolean" ||
        !context || typeof context.session_id !== "string" ||
        context.session_id !== bundle.run?.run_id ||
        context.execution_id !== bundle.run?.execution_id ||
        !Array.isArray(changes) || !changes.every(validEntry)) {
      throw new Error("Invalid or foreign native review contract.");
    }
    const identities = new Set(changes.map((entry) => JSON.stringify(entry)));
    for (const [key, count] of [
      ["project_changes", "project"], ["environment_changes", "environment"],
      ["unclassified_changes", "unclassified"], ["unsupported_project_changes", "unsupported_project"],
    ]) {
      const entries = handoff[key];
      if (!Array.isArray(entries) || !entries.every((entry) =>
        identities.has(JSON.stringify(entry))) ||
          (handoff.classification_available || count === "unclassified"
            ? handoff.counts?.[count] !== entries.length
            : handoff.counts?.[count] !== null)) {
        throw new Error("Invalid native scope inventory.");
      }
    }
    const partition = [
      ...handoff.project_changes, ...handoff.environment_changes, ...handoff.unclassified_changes,
    ].map((entry) => JSON.stringify(entry)).sort();
    if (JSON.stringify(partition) !== JSON.stringify(changes.map((entry) => JSON.stringify(entry)).sort())) {
      throw new Error("Incomplete native scope inventory.");
    }
    const counts = { total: changes.length, file: 0, directory: 0, symlink: 0, other: 0, unknown: 0 };
    for (const change of changes) {
      const type = change.kind === "deleted" ? change.old_type : change.new_type;
      counts[type ?? "unknown"]++;
    }
    return {
      review_status: "complete", changes, counts,
      in_scope: handoff.counts.project, outside_scope: handoff.counts.environment,
      unclassified: handoff.counts.unclassified,
      handoff, review_context: context, filesystem_state: bundle.filesystem.state,
    };
  } catch (error) {
    return unavailable(error.message);
  }
}
