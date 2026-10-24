# Engine semantics 7

Review the implementation as well as this contract before approving @engine.

- Input is one or more explicitly supplied Markdown files/directories, with no
  default search path. Overlapping files are loaded once; distinct files with
  duplicate IDs fail. Their nearest common directory is the review root unless --root explicitly fixes it.
  Explicit case selections must be unique and belong to the selected domain.
  One-line semspec: case comments with unique id=S-DOMAIN-NNN mark the immediately
  following Bash fence. Headings and unmarked examples do not declare tests.
  timeout and paired xfail parameters are validated; multiple cases may share a
  section. Case review binds its entire enclosing heading section (or document).
  semspec: setup comments mark preparation: sibling index.md first, then the
  case document, each in source order. Full preparation documents are sealed.
  Configuration input and legacy heading/semantic-case discovery are unsupported.
- Digests follow DESIGN.md §8: trim line-end whitespace, remove trailing
  blank lines, append one LF, no Unicode normalization, SHA-256 domain separation.
  Case digests bind the complete case, sorted vocabulary digests and engine version.
  A second domain-separated digest binds exact extracted executable bytes and
  expected stream cells. Preparation scripts likewise bind exact bytes. Prose
  normalization must never make executable whitespace invisible to review.
- Review is independent from verdict. Require-reviewed checks the case, all used
  vocabulary and engine, including SKIP/XFAIL. Reapproval replaces the current
  digest; Git preserves review history.
- Bash checks run with set -euo pipefail, sourced vocabulary, null stdin,
  inherited environment plus reserved runner variables.
  Preparation is extracted from original Markdown, bound by the execution digest,
  written into the case directory and sourced in the fixed index/local order.
  CASE_ROOT/ws is a fresh cwd. Workspace snapshots/logs live outside ws. Bash -n
  checks syntax; source/dot commands are allowed. Checks are trusted code and
  additional sourced files need project review; this is not a security sandbox.
- A new process group contains each check. Timeout or CLI SIGINT/SIGTERM sends TERM, then KILL
  after five seconds. On ordinary exit remaining group descendants are also killed.
  Normal descendants cannot keep logs alive. Detached processes are outside this
  group; do not detach persistent processes in specifications.
- Without timeout, 0 means PASS and 77 means SKIP (output explains why). Other
  nonzero exits, signals or timeout mean FAIL. SKIP takes precedence over xfail. Expected failures map only
  ordinary nonzero exits to XFAIL and success to XPASS, never signals, timeout,
  command-not-found/not-executable (126/127), launch or engine errors. XPASS fails the run.
- Default timeout is 180s, --timeout sets the default, comment timeout wins.
  Markdown input needs no subject setting; checks resolve commands through PATH.
  Explicit subject paths must be available executables. The optional review ledger
  lives at that review root; init requires an explicit output directory and
  creates only Markdown, never an approval ledger.
- Prerequisites are ordinary Bash checks, without probes or requires annotations. Unknown configuration arguments,
  unknown cases/domains, malformed specs and unavailable subject paths exit 2.
  Reports must contain exactly the selected IDs, once each, with a nonempty inventory.
  --require-pass rejects every non-PASS, including SKIP/XFAIL, independently of review.
  ERROR has exit 3 precedence; FAIL/XPASS/inventory errors/strict gate failures exit 1,
  otherwise 0. After valid selection, --output clears a previous report before
  execution and atomically writes a fresh report, including failures. Invalid
  selection preserves existing output; launch failure/interruption leaves no stale
  report. Output cannot replace loaded specifications, preparation or the ledger.
- FAIL/XFAIL/XPASS/ERROR always retain their case directory; --keep retains every executed
  case. Outputs are human or JSON. v0.1 is serial; signing/JUnit/parallelism are rejected.
- tree-state includes relative paths, types, permissions, full SHA-256 contents and
  symlink targets, sorted by raw path bytes, without following links. Paths/targets
  are JSON quoted byte strings (non-UTF8 bytes use \u00xx), including the root
  directory as ".", excluding mtime, atime and inode. Unsupported file kinds are named, not treated as regular files.
- json-get requires a present RFC6901 pointer and emits canonical JSON, including
  quoted strings; root pointer is empty. diff emits a unified diff and returns 1 for
  differences, 0 for equality. assert_content compares exact bytes, including final LF.
- Approval requires stdin and stdout TTY, shows current sealed content/dependencies,
  and requires typing the exact item. No automatic approval path. The ledger is
  atomically written under a file lock. No approved snapshots, diff/revoke commands
  or retired list; Git holds differences and history.

Changes to these execution, helper, verdict, digest or review semantics require
incrementing ENGINE_SEMANTICS and updating the digest stability tests.

- `expect case=ID stream=stdout|stderr` marks a text cell whose exact extracted
  bytes must match the corresponding captured stream after zero exit. Duplicates
  and unknown case references are errors. Evidence records digest, exit, timeout
  and up to 64 KiB per stream; comparisons use full stream files. `case.log` is a
  stdout-then-stderr preview; it does not preserve cross-stream ordering.
- `filter` is read-only unless --execute is explicit. It emits Markdown or a
  versioned JSON document with dependencies, original-byte edits and diagnostics.
  NOT RUN and STALE are not PASS. Saved evidence must match source, preparation
  and engine; rendered case prose and executable cells must match canonical input.
  --execute needs no saved report, requires PASS, and never approves. --artifacts
  explicitly retains a report and case logs; --results reads trusted local evidence.
  --stdin execution requires canonical source bytes. CLI cancellation exits 128+signal.
- `bench id=NAME` marks a TOML direct-argv experiment plan. The shared Python
  perf_counter_ns runtime runs warmups and samples, requires zero exits and all
  validation/hooks, always attempts cleanup, retains every trial and reports sample
  statistics. Plan/source/runtime bytes bind evidence. These are observations, not
  case approvals; existing registered benchmark publication gates remain mandatory.
