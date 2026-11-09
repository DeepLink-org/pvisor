/**
 * Derive a display-only per-file view from the public native manifest and text
 * review. Never read upper trees or use parsed text as apply/export authority.
 * Hunk lengths, not header-looking file content, delimit unified diff bodies.
 */
export function fileReview(review) {
  const changes = review.handoff?.project_changes ?? review.changes;
  if (review.review_status !== "complete" || !Array.isArray(changes)) return null;
  const files = changes.map((change) => ({ ...change,
    diff_status: (change.new_type ?? change.old_type) === "directory" ? "directory" : "unavailable",
    additions: null, deletions: null, diff: null }));
  if (review.diff_status === "truncated") {
    for (const file of files) if (file.diff_status !== "directory") file.diff_status = "partial";
    return files;
  }
  if (typeof review.stdout !== "string" || review.diff_status !== "separate_observation") return files;
  // Native labels/diagnostics are not a lossless framing protocol for control
  // characters or non-UTF-8 names. Keep those inventories, but do not guess.
  if (changes.some((change) => /[\x00-\x1f\x7f]/.test(change.path) ||
      (change.path_bytes && !Buffer.from(change.path_bytes).equals(Buffer.from(change.path))))) return files;
  const byPath = new Map();
  for (const file of files) {
    if (byPath.has(file.path)) return files;
    byPath.set(file.path, file);
  }
  const lines = review.stdout.split("\n");
  const start = lines.indexOf("Project diff (environment changes: use review --json)");
  if (start < 0) return files;
  let current = null;
  let oldLeft = 0;
  let newLeft = 0;
  let added = 0;
  let removed = 0;
  let hunks = 0;
  let body = [];
  let invalid = false;
  let truncated = false;
  const seen = new Set();
  function finish() {
    if (!current) return;
    const complete = !invalid && oldLeft === 0 && newLeft === 0 && hunks > 0;
    current.diff_status = complete ? "text" : "partial";
    current.additions = complete ? added : null;
    current.deletions = complete ? removed : null;
    current.diff = body.join("\n");
    current = null;
  }
  for (let index = start + 1; index < lines.length; index++) {
    const line = lines[index];
    if (current && (oldLeft > 0 || newLeft > 0)) {
      if (line.startsWith("\\ ")) { body.push(line); continue; }
      const prefix = line[0];
      if ((prefix === " " && oldLeft > 0 && newLeft > 0) ||
          (prefix === "-" && oldLeft > 0) || (prefix === "+" && newLeft > 0)) {
        body.push(line);
        if (prefix !== "+") oldLeft--;
        if (prefix !== "-") newLeft--;
        if (prefix === "+") added++;
        if (prefix === "-") removed++;
        continue;
      }
      invalid = true;
      finish();
    }
    if (/^  … diff output truncated at \d+ bytes$/.test(line)) {
      truncated = true;
      finish();
      break;
    }
    const hunk = /^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@(?:.*)$/.exec(line);
    if (current && hunk) {
      oldLeft = Number(hunk[1] ?? 1);
      newLeft = Number(hunk[2] ?? 1);
      hunks++;
      body.push(line);
      continue;
    }
    if (current && line.startsWith("\\ ")) { body.push(line); continue; }
    if (line.startsWith("--- a/") && lines[index + 1] === `+++ b/${line.slice(6)}`) {
      finish();
      const file = byPath.get(line.slice(6));
      if (!file || file.diff_status === "directory" || seen.has(file.path)) {
        if (file && seen.has(file.path)) {
          file.diff_status = "unavailable"; file.additions = null; file.deletions = null; file.diff = null;
        }
        index++;
        continue;
      }
      seen.add(file.path);
      current = file;
      oldLeft = 0; newLeft = 0; added = 0; removed = 0; hunks = 0; invalid = false;
      body = [line, lines[++index]];
      continue;
    }
    finish();
    // Rust's debug path string agrees with JSON for ordinary display names.
    // A format mismatch remains unavailable rather than being misclassified.
    const binarySuffix = ": binary (text diff unavailable)";
    if (line.endsWith(binarySuffix)) {
      try {
        const file = byPath.get(JSON.parse(line.slice(0, -binarySuffix.length)));
        if (file && file.diff_status !== "directory") file.diff_status = "binary";
      } catch { /* Unknown native diagnostic: no invented classification. */ }
    }
  }
  finish();
  if (truncated) {
    for (const file of files) {
      if (file.diff_status === "directory" || file.diff_status === "binary") continue;
      file.diff_status = "partial";
      file.additions = null; file.deletions = null;
    }
  }
  return files;
}
