# Engine semantics 2

Review the implementation as well as this contract before approving @engine.

- Markdown h3 cases stop at the next h1/h2/h3, excluding headings in code fences.
  Each has a unique S-DOMAIN-NNN ID, nonempty 语义/违反示例 and exactly one bash check.
- Digests follow docs/semspec-design.md §8: trim line-end whitespace, remove trailing
  blank lines, append one LF, no Unicode normalization, SHA-256 domain separation.
  Case digests bind the complete case, sorted vocabulary digests and engine version.
  Executed checks and vocabulary use those same normalized bytes.
- Review is independent from verdict. Require-reviewed checks the case, all used
  vocabulary and engine, including SKIP/XFAIL. Reapproval replaces the current
  digest; Git preserves review history.
- Bash checks run with set -euo pipefail, sourced vocabulary, null stdin,
  inherited environment plus configured variables and reserved runner variables.
  Vocabulary is copied from hashed bytes into the case directory and sourced in
  filename order, independent of configuration order (digests bind the set).
  CASE_ROOT/ws is a fresh cwd. Workspace snapshots/logs live outside ws. Bash -n
  checks syntax; source/dot commands are allowed. Checks are trusted code and
  additional sourced files need project review; this is not a security sandbox.
- A new process group contains each check. Timeout sends TERM, then KILL
  after five seconds. On ordinary exit remaining group descendants are also killed.
  Normal descendants cannot keep logs alive. Detached processes are outside this
  group; do not detach persistent processes in specifications.
- Without timeout, 0 means PASS and 77 means SKIP (output explains why). Other
  nonzero exits, signals or timeout mean FAIL. SKIP takes precedence over xfail. Expected failures map only
  these results to XFAIL/XPASS, never launch or engine errors. XPASS fails the run.
- Prerequisites are ordinary Bash checks, without probes or requires annotations. Invalid configuration,
  unknown cases/domains, malformed specs and unavailable subject paths exit 2.
  ERROR has exit 3 precedence; FAIL/XPASS/unreviewed-required exit 1, otherwise 0.
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
