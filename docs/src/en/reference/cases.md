# pVisor Job scenarios and regression examples

Start with a simple Job, add limits, staging, VM, container and networking, then complete review, branching and trajectory replay. Each case explains purpose, preparation and expected results before executable commands. IDs allow individual regressions.

`run` creates a Job; `status`, `inspect`, `apply`, `drop`, `fork` and `kill` operate on it directly. `replay` creates a Job from a trajectory. Labels such as A01 identify regressions and reports.

## Choose by need

| Need | Start here |
| --- | --- |
| Run a command or verify default writes | A01–A03, A07 |
| Timeout, memory or file limits | A04, B01–B04 |
| Keep, discard or inspect file changes | C01–C06 |
| Compose OverlayFS layers or grant path permissions | D01, D04–D06 |
| Understand default host isolation | D02–D03 |
| VM, host rootfs or OCI images | E01–E06 |
| Native OCI containers | F01–F04 |
| Proxy configuration or network denial | G01–G07 |
| Gateway or trajectory recording | H01–H02 |
| Configuration files or RunSpec | I01–I03 |
| Combine capabilities | J01–J03 |
| Review, selective apply, fork or terminate | K01–K04 |
| Prepare trajectory replay | M01 |
| Terminal UI and permission prompts | D06, M02 |

This appendix is both user documentation and the DOC semspec source. Each scenario includes purpose, semantics, a violation example, commands and assertions. `just cases` executes the Bash checks below. Original A01–M02 labels remain in headings.

## How to use

For manual product commands, prepare a test working directory and ensure `pvisor` is on `PATH`. Full check blocks use runner fixtures/assertion functions; execute them with `just cases`.

```bash
mkdir -p /tmp/pvisor-cases/workspace
cd /tmp/pvisor-cases/workspace
```

Run automated checks from the repository root:

```bash
just semspec list --domain DOC
just semspec run docs/src/zh/reference/cases.md --subject-bin target/release/pvisor
just cases --case S-DOC-001,S-DOC-012 --keep
just cases
just semspec show S-DOC-001
```

`just cases` builds release pVisor, discovers the 54 active DOC specifications on this page and [six VM control and RAM backing cases](cases-vm.md) and writes JSON to `target/pvisor-case-report.json`. Select by S-DOC ID: A01 is S-DOC-001, C01 is S-DOC-012. See `tests/semantics/README.md` for the full mapping. An existing binary can be used with `just semspec run --domain DOC --subject-bin PATH`. L01/L02 and S-DOC-053/S-DOC-054 were removed with `env`; their IDs are retired and never reused.

Each specification includes original commands, expected exits and all assertions in one review digest. Expected nonzero exits must actually occur and satisfy the original assertions; they are not xfail. The reviewed assertion vocabulary comes from `cases.sh`, which reads this case's Bundle, run record and command logs. Configuration, Jobs and fixtures live in temporary CASE_ROOT. Failures retain their workspace; `--keep` retains every workspace. Missing declared prerequisites report SKIP.

New cases remain UNREVIEWED. Passing checks are separate from human approval. Use `just cases --require-reviewed` as a gate only after humans review cases, vocabulary and engine. See `just semspec run --help` for supported options.

| Test resource | Environment variable |
| --- | --- |
| Linux rootfs | `PVISOR_CASE_ROOTFS`; Linux defaults to host `/` if unset, testing the path rather than an independent guest rootfs |
| VM image | `PVISOR_CASE_IMAGE`; default `ubuntu:latest` |
| Container image | `PVISOR_CASE_CONTAINER_IMAGE`; default `ubuntu:latest`, compatible with dynamic pVisor ABI |
| Automatically selected OCI runtime | `PVISOR_CASE_CONTAINER_RUNTIME`; otherwise crun then runc; F04 explicitly requires runc |
| VM agent | `PVISOR_CASE_AGENT`; must execute inside the guest |

Linux host staging requires user/mount namespaces and FUSE; macOS requires working macFUSE. These VM cases require Linux and `/dev/kvm`. An OCI executable alone does not prove permission to run containers. Actual execution/assertions still verify these requirements rather than treating them as passes.

## Executable cases

## A. Basic invocation and identity

Start with A01. Use subsequent examples only when you need a fixed display name, captured output or explicit environment projection.

### S-DOC-001: A01 Minimal invocation without run

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: First use: verify commands and Job identity.

**Semantics**: Print the absolute current workspace path and exit successfully. Default executor is host, without staging; networking is ambient when no policy is configured.

**Rationale**: Run in the current directory without explicitly writing `run`. Everything after `--` is the agent command and arguments.

**Violation example**: Exit 0 while printing the parent directory, or start staging by default.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor -- /bin/pwd
CASE_COMMAND

stdout_has "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
bundle_expect run.state completed
bundle_expect run.exit_code 0
bundle_expect run.agent pwd
bundle_expect network.policy.mode ambient
```

### S-DOC-002: A02 Explicit run equals the short form

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: First use: verify commands and Job identity.

**Semantics**: Both output files contain the same current working directory. Each execution has a separate record; Job IDs and times may differ.

**Rationale**: Compare explicit/implicit `run`, saving agent stdout separately.

**Violation example**: Working directory output differs between the two forms.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor -- /bin/pwd > implicit.txt
pvisor run -- /bin/pwd > explicit.txt
CASE_COMMAND

diff implicit.txt explicit.txt
test "$(cat implicit.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
bundle_expect run.agent pwd
```

### S-DOC-003: A03 Job name and stdio capture

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: First use: verify commands and Job identity.

**Semantics**: Job name is `smoke`, result stdout is `hello` and is not truncated.

**Rationale**: Name the Run with `--name smoke` and retain output with `--stdio capture`.

**Violation example**: Lose the agent name or mark hello truncated.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor --name smoke --stdio capture -- /bin/sh -c 'printf hello'
CASE_COMMAND

bundle_expect run.agent smoke
bundle_expect run.output.stdout hello
bundle_expect run.output.stdout_truncated false
```

### S-DOC-004: A04 Timeout

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: First use: verify commands and Job identity.

**Semantics**: pVisor exits nonzero and records failure type `deadline_exceeded`.

**Rationale**: Set a wall-clock timeout. `100ms` is elapsed duration from start, not CPU time; the command deliberately sleeps 10 seconds.

**Violation example**: The timed-out command succeeds or records an ordinary retryable process_exit failure.

```bash
require_python3
case_setup
case_run nonzero <<'CASE_COMMAND'
pvisor --timeout 100ms -- /bin/sleep 10
CASE_COMMAND

bundle_expect run.state failed
bundle_expect run.failure.kind deadline_exceeded
bundle_expect run.failure.retryable false
```

### S-DOC-005: A05 Strict mode rejects best-effort boundaries

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: First use: verify commands and Job identity.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Current host/container/VM paths reject the request before launching the agent because Subprocess enforcement evidence is missing (`UnsupportedPolicy`). This verifies fail-closed behavior; it does not mean a stronger sandbox is currently ready under `--strict` on any executor.

**Rationale**: Require strict capability checks and deny networking. `--strict` rejects requested capabilities without enforcement evidence.

**Violation example**: Launch despite missing evidence, or reject without naming the missing capability.

```bash
require_python3
case_setup
case_run nonzero <<'CASE_COMMAND'
pvisor --strict --overlaynet-deny-all -- "$CASE_TRUE"
CASE_COMMAND

stdout_has "lacks enforced evidence for requested capability dimensions"
```

### S-DOC-006: A06 Explicit environment projection

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: First use: verify commands and Job identity.

**Semantics**: The child sees `TEST_PVISOR_VALUE=visible`; records list the variable without claiming full host environment inheritance.

**Rationale**: Set a variable only for this command and explicitly project it through `--pass-env`.

**Violation example**: Omit the permitted variable or incorrectly claim complete host inheritance.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
TEST_PVISOR_VALUE=visible pvisor --pass-env TEST_PVISOR_VALUE -- /usr/bin/env
CASE_COMMAND

stdout_has "TEST_PVISOR_VALUE=visible"
bundle_contains environment.projected_keys TEST_PVISOR_VALUE
bundle_expect environment.inherits_host false
```

### S-DOC-007: A07 Default writes reach the workspace

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

**Semantics**: After exit, `direct.txt` exists in the original workspace and records contain no OverlayFS stage.

**Rationale**: Verify the ordinary host Job default writable lower without selecting another executor/stage.

**Violation example**: A host write without a staging request does not reach the workspace.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor -- /bin/sh -c 'printf direct > direct.txt'
CASE_COMMAND

test "$(cat direct.txt)" = direct
record_expect overlay null
```

## B. Resource limits

These cases distinguish requested limits from actual enforcement. B01 inspects complete configuration; B02 actually tries to exceed a file limit.

### S-DOC-008: B01 Combine all resource limits

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Control or verify resource limits.

**Semantics**: The command succeeds and all five requested values appear in records alongside effective limits/mechanisms. `/bin/true` does not consume the allowances; this does not test exceeding them.

**Rationale**: Combine memory, process count, CPU time, open file count and single-file size. MiB is binary; `--max-cpu-time` differs from wall-clock timeout.

**Violation example**: Accept options but record incorrect requests, or omit the effective file-size limit/rlimit mechanism.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor \
  --memory 256MiB \
  --max-processes 32 \
  --max-cpu-time 5s \
  --max-open-files 128 \
  --max-file-size 1MiB \
  -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect resources.requested.memory_bytes 268435456
bundle_expect resources.requested.processes 32
bundle_expect resources.requested.cpu_time_ms 5000
bundle_expect resources.requested.open_files 128
bundle_expect resources.requested.file_size_bytes 1048576
bundle_expect resources.effective.file_size_bytes 1048576
bundle_contains resources.mechanisms rlimit
```

### S-DOC-009: B02 File-size limit is enforced

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Control or verify resource limits.

**Semantics**: Writing fails; any file left on disk is at most 1024 bytes. The result records a process-exit failure.

**Rationale**: Set 1KiB and use `dd` to attempt a 4KiB write.

**Violation example**: dd writes 4KiB successfully or leaves more than 1024 bytes after failure.

```bash
require_python3
case_setup
case_run nonzero <<'CASE_COMMAND'
pvisor --max-file-size 1KiB -- /bin/sh -c 'dd if=/dev/zero of=large bs=4096 count=1'
CASE_COMMAND

bundle_expect resources.requested.file_size_bytes 1024
bundle_expect run.state failed
bundle_expect run.failure.kind process_exit
test ! -f large || [ "$(wc -c < large)" -le 1024 ]
```

### S-DOC-010: B03 Short memory alias

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Control or verify resource limits.

**Semantics**: The command succeeds and requests 268435456 bytes, equal to `--memory 256MiB`.

**Rationale**: Use `--mem`, an alias for `--memory`, with a simple command.

**Violation example**: Parse --mem 256MiB differently from --memory.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor --mem 256MiB -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect resources.requested.memory_bytes 268435456
```

### S-DOC-011: B04 Total stage size limit

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Control or verify resource limits.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: The stage is created and retained at the selected path. This verifies option acceptance/directory creation only; artifacts do not currently record the limit, and the case does not fill the stage.

**Rationale**: Request a 1GiB total stage limit, distinct from B02 single-file size.

**Violation example**: The command succeeds without staged filesystem state or with the wrong storage path.

```bash
require_python3
require_stage
case_setup
case_run success <<'CASE_COMMAND'
pvisor --stage "$CASE_ROOT/limited-stage" --overlayfs-max-size 1GiB -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect filesystem.state staged
bundle_expect safety.filesystem_changes_staged true
record_expect storage "$(realpath "$PVISOR_CASE_ROOT/limited-stage")"
```

## C. Staging and whole-rootfs

Use these when an agent should modify files without changing the current workspace. C01 is explicit persistent staging; C02 lets `--safe` select/retain the stage; C03 drops a persistent stage explicitly.

### S-DOC-012: C01 Persistent stage

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Isolate file changes, retain a stage or verify whole-rootfs behavior.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Original workspace has no `result.txt`; the change list includes it and the selected stage retains `run-bundle.json` for later inspection.

**Rationale**: Create result.txt in the workspace view and retain its changes in a stage.

**Violation example**: result.txt reaches the original workspace or is omitted from changes.

```bash
require_python3
require_stage
case_setup
case_run success <<'CASE_COMMAND'
pvisor --stage "$CASE_ROOT/stage-keep" -- /bin/sh -c 'printf changed > result.txt'
CASE_COMMAND

bundle_expect filesystem.state staged
bundle_contains filesystem.changes result.txt
bundle_expect safety.filesystem_write_non_bypassable true
test ! -e result.txt
test -f "$PVISOR_CASE_ROOT/stage-keep/run-bundle.json"
```

### S-DOC-013: C02 Retain the stage by default

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Isolate file changes, retain a stage or verify whole-rootfs behavior.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: The command succeeds; the logged storage directory/Bundle remain and the new file is absent from the original workspace.

**Rationale**: Without `--stage`, `--safe` uses persistent Job storage and retains changes after exit.

**Violation example**: The logged Bundle disappears after exit or result.txt reaches the original workspace.

```bash
require_python3
require_stage
case_setup
case_run success <<'CASE_COMMAND'
pvisor --safe -- /bin/sh -c 'printf changed > result.txt'
CASE_COMMAND

storage=$(dirname "$(grep -m1 '^Run Bundle: ' "$PVISOR_CASE_STDOUT" | cut -d' ' -f3-)")
test -n "$storage"
test -f "$storage/run-bundle.json"
test ! -e result.txt
```

### S-DOC-014: C03 Explicitly discard a persistent stage

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Isolate file changes, retain a stage or verify whole-rootfs behavior.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: The stage directory remains; filesystem state becomes `discarded`; no new file appears in the original workspace.

**Rationale**: Select a persistent stage then discard changes using `pvisor drop` after completion.

**Violation example**: Records stay staged after drop or result.txt reaches the original workspace.

```bash
require_python3
require_stage
case_setup
case_run success <<'CASE_COMMAND'
pvisor --stage "$CASE_ROOT/stage-drop" -- /bin/sh -c 'printf changed > result.txt'
pvisor drop "$CASE_ROOT/stage-drop"
CASE_COMMAND

record_expect overlay.state discarded "$PVISOR_CASE_ROOT/stage-drop"
test ! -e result.txt
test -f "$PVISOR_CASE_ROOT/stage-drop/run-bundle.json"
```

### S-DOC-015: C04 Preserve existing stage directory contents

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Isolate file changes, retain a stage or verify whole-rootfs behavior.

**Semantics**: The command succeeds, retaining existing `user-file` and the new Bundle in the selected directory.

**Rationale**: `--stage PATH` always denotes persistent storage; cleanup must not delete existing user files.

**Violation example**: Cleanup deletes the original user-file.

```bash
require_python3
require_stage
case_setup
case_run success <<'CASE_COMMAND'
mkdir -p "$CASE_ROOT/existing-stage"
touch "$CASE_ROOT/existing-stage/user-file"
pvisor --stage "$CASE_ROOT/existing-stage" -- "$CASE_TRUE"
CASE_COMMAND

test -f "$PVISOR_CASE_ROOT/existing-stage/user-file"
test -f "$PVISOR_CASE_ROOT/existing-stage/run-bundle.json"
```

### S-DOC-016: C05 Whole-rootfs capture and tmpfs isolation

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Isolate file changes, retain a stage or verify whole-rootfs behavior.

Preparation: Linux user/mount namespaces; macOS Seatbelt does not provide this whole-rootfs/tmpfs isolation.

**Semantics**: Workspace changes appear in stage; neither host workspace nor host `/tmp` gets new files. This does not verify persistence of ordinary rootfs paths outside the workspace.

**Rationale**: Compare retained workspace writes with temporary sandbox-directory writes used only for this Run.

**Violation example**: Workspace changes are not staged or sandbox /tmp writes reach host /tmp.

```bash
require_python3
require_rootless
case_setup
case_run success <<'CASE_COMMAND'
pvisor --stage "$CASE_ROOT/root-stage" -- /bin/sh -c \
  'printf workspace > ./workspace-change; printf tmp > "$1"' sh "$CASE_TMP_PATH"
CASE_COMMAND

bundle_contains filesystem.changes workspace-change
test ! -e workspace-change
test ! -e "$CASE_TMP_PATH"
```

### S-DOC-017: C06 Safe isolates HOME writes

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Preparation: Linux user/mount namespaces available.

**Semantics**: The agent reads its new HOME state; host HOME has no such file; persistent workspace stage remains reviewable.

**Rationale**: Check `--safe` provides a separate HOME copy-on-write view as well as workspace staging. A dedicated test HOME avoids the real user directory.

**Violation example**: Write HOME/state to host HOME or prevent the agent from reading its own write.

```bash
require_python3
require_rootless
case_setup
case_run success <<'CASE_COMMAND'
mkdir -p "$CASE_ROOT/home"
HOME="$CASE_ROOT/home" pvisor --safe --stage "$CASE_ROOT/safe-home" -- \
  /bin/sh -c 'printf private > "$HOME/state"; cat "$HOME/state"'
CASE_COMMAND

stdout_has private
test ! -e "$PVISOR_CASE_ROOT/home/state"
bundle_expect filesystem.state staged "$PVISOR_CASE_ROOT/safe-home"
bundle_expect network.policy.mode allowlist "$PVISOR_CASE_ROOT/safe-home"
```

## D. OverlayFS and host security boundaries

D01 covers layer composition; D02/D03 host execution; D04–D06 denial, direct writes and interactive permission. File rules `deny`, `ask`, `warn` mean reject, prompt, and allow with warning; they append by default. `--mount` mode `read` grants a read-only host share (host plus `--safe`/`--ask` required), `stage` composes copy-on-write lowers, and `write` writes directly to host lower. `--access PATH-GLOB:ask` automatically enables audit TUI and safe staging without another `--ask`.

In file prompts, `1` grants this file, `2` grants sibling-directory files, `3` grants the same suffix, and `d` denies this target. Explicit deny rules reject without prompting. Scope defaults to session. Press `s`/`w`/`u` for session/workspace/user, then a number for permission range and Enter to confirm. Deny is selected by default; `d` rejects immediately.

Session rules go to the Job's `audit-policy.json`. Workspace/user rules go to `permissions` in `~/.config/pvisor/config.toml`, or an absolute `XDG_CONFIG_HOME`. Workspace identity uses canonical working directories. Later TUI Jobs load them with session > workspace > user precedence and the last matching rule in each layer. Persisted file paths use original absolute paths; user suffix rules can cover other workspaces, so consider scope.

In Permissions, select with j/k and press x twice to remove a decision; another rule or a prompt may then apply. `audit.jsonl` records manual/automatic decisions and saved scopes. Job records persist by default; `--stage PATH` chooses their location.

### S-DOC-018: D01 Advanced OverlayFS composition

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Inspect OverlayFS views and host security boundaries.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: The recorded target is `view`; lowers are `layer`, `base`, then the Run-owned workspace snapshot, top to bottom. Empty directories test configuration order, not same-name override contents.

**Rationale**: Layer two host directories into the workspace view and choose the agent-visible path. `directory` selects that backend; only explicit apply commits changes.

**Violation example**: Reverse layer/base order or omit the workspace snapshot directory.

```bash
require_python3
require_stage
case_setup
case_run success <<'CASE_COMMAND'
mkdir -p "$CASE_ROOT/base" "$CASE_ROOT/layer" "$PWD/view"
pvisor \
  --stage "$CASE_ROOT/composed-stage" \
  --mount "$CASE_ROOT/base:$PWD/view:stage" \
  --mount "$CASE_ROOT/layer:$PWD/view:stage" \
  -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect filesystem.state staged
record_expect overlay_lowers.0 "$(realpath "$PVISOR_CASE_ROOT/layer")"
record_expect overlay_lowers.1 "$(realpath "$PVISOR_CASE_ROOT/base")"
record_contains overlay_lowers.2 "$PVISOR_CASE_ROOT/composed-stage/.overlay-lowers/"
test -d "$(record_get overlay_lowers.2)"
```

### S-DOC-019: D02 Explicit host executor

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Inspect OverlayFS views and host security boundaries.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Linux records `rootless_process`; macOS records `sandboxed_process`; neither may degrade to host process.

**Rationale**: Explicitly choose host and inspect current-platform isolation type.

**Violation example**: Downgrade to host_process while recording isolation success.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor --executor host -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect run.executor.kind process
if [ "$(uname -s)" = "Darwin" ]; then
  bundle_expect run.executor.isolation sandboxed_process
else
  bundle_expect run.executor.isolation rootless_process
fi
bundle_expect safety.host_process false
```

### S-DOC-020: D03 Host stage hides the original workspace path

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Inspect OverlayFS views and host security boundaries.

Preparation: Linux user/mount namespaces; macOS lacks this procfs/mount-namespace path-hiding behavior.

**Semantics**: cwd points to stage merged, and output omits the original workspace path. This checks displayed paths only, not denial of all original-path/inherited-FD access.

**Rationale**: Inspect child cwd/procfs under staging, saving three command outputs to views.txt.

**Violation example**: procfs cwd exposes the original workspace path.

```bash
require_python3
require_rootless
case_setup
case_run success <<'CASE_COMMAND'
pvisor --stage "$CASE_ROOT/host-stage" -- /bin/sh -c \
  'pwd; readlink /proc/self/root; readlink /proc/self/cwd' > views.txt
CASE_COMMAND

merged="$PVISOR_CASE_ROOT/host-stage/merged"
test "$(sed -n 1p views.txt)" = "$merged"
test "$(sed -n 3p views.txt)" = "$merged"
! grep -Fq -- "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)" views.txt
```

### S-DOC-021: D04 Deny sensitive file reads explicitly

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Preparation: Linux user/mount namespaces available.

**Semantics**: Read fails, the deny rule is recorded, and host content remains unchanged.

**Rationale**: Use `--access PATH-GLOB:deny` for matching workspace files.

**Violation example**: Allow private/token or omit the denied target from review.

```bash
require_python3
require_rootless
case_setup
case_run nonzero <<'CASE_COMMAND'
mkdir -p private
printf secret > private/token
pvisor --stage "$CASE_ROOT/access-stage" --access 'private/**:deny' -- \
  /bin/cat private/token
CASE_COMMAND

stdout_has 'pVisor file access denied'
bundle_expect filesystem.access_policy.deny.0 'private/**'
bundle_contains run_observation.filesystem.paths private/token
test "$(cat private/token)" = secret
```

### S-DOC-022: D05 Direct writes to an explicit share

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Preparation: Linux user/mount namespaces available.

**Semantics**: Shared directory file `out` is written directly to host lower without apply.

**Rationale**: Grant a writable external host directory with `--mount SOURCE:write`.

**Violation example**: An explicit write share fails to receive mounted content.

```bash
require_python3
require_rootless
case_setup
case_run success <<'CASE_COMMAND'
mkdir -p "$CASE_ROOT/shared"
pvisor --mount "$CASE_ROOT/shared":write -- \
  /bin/sh -c 'printf mounted > "$1"' sh "$CASE_ROOT/shared/out"
CASE_COMMAND

test "$(cat "$PVISOR_CASE_ROOT/shared/out")" = mounted
```

### S-DOC-023: D06 Ask prompt and directory permission for this Job

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Preparation: Linux user/mount namespaces and Python 3. A pseudo-terminal sends 2/Enter automatically; manually choose and confirm with Enter.

**Semantics**: Only one file prompt appears; both files are readable. audit-policy.json stores the directory rule and audit.jsonl records the second automatic allow. Stage retains this Job audit without turning it into global configuration.

**Rationale**: Start audit TUI with `--access 'private/*.txt:ask'`; press 2/Enter on first read, then read a sibling to verify reuse.

**Violation example**: Prompt again for the sibling or persist directory permission with the wrong scope.

```bash
require_python3
require_rootless
case_setup
case_run success <<'CASE_COMMAND'
mkdir -p private
printf ASK_ONE > private/one.txt
printf ASK_TWO > private/two.txt
python3 - <<'PY'
import fcntl, os, pty, select, signal, struct, time

pid, master = pty.fork()
if pid == 0:
    os.environ['TERM'] = 'xterm-256color'
    os.execvp('pvisor', [
        'pvisor', '--no-agent-defaults', '--stage', os.environ['CASE_ROOT'] + '/ask-stage',
        '--access', 'private/*.txt:ask', '--', '/bin/sh', '-c',
        'cat private/one.txt; sleep 1; cat private/two.txt',
    ])
fcntl.ioctl(master, 0x5414, struct.pack('HHHH', 24, 100, 0, 0))
screen = bytearray()
prompted = False
status = None
deadline = time.monotonic() + 25
try:
    while time.monotonic() < deadline:
        ready, _, _ = select.select([master], [], [], 0.1)
        if ready:
            try:
                screen.extend(os.read(master, 65536))
            except OSError:
                pass
        if not prompted and b'FILE ACCESS PAUSED' in screen:
            os.write(master, b'2\r')
            prompted = True
        ended, result = os.waitpid(pid, os.WNOHANG)
        if ended:
            status = result
            break
    if status is None:
        os.killpg(pid, signal.SIGTERM)
        _, status = os.waitpid(pid, 0)
        raise RuntimeError('timed out waiting for the Job')
finally:
    os.close(master)
assert prompted and os.waitstatus_to_exitcode(status) == 0
assert b'ASK_ONE' in screen and b'ASK_TWO' in screen
print('ASK directory grant reused')
PY
CASE_COMMAND

stdout_has 'ASK directory grant reused'
python3 - <<'PY'
import json, os
from pathlib import Path

stage = Path(os.environ['CASE_ROOT'] + '/ask-stage')
policy = json.loads((stage / 'audit-policy.json').read_text())
assert any(rule['kind'] == 'file' and rule['scope'] == 'directory'
           and rule['value'] == 'private' and rule['decision'] == 'allow'
           for rule in policy['rules'])
decisions = [json.loads(line) for line in (stage / 'audit.jsonl').read_text().splitlines()]
assert any(item['request']['target'] == 'private/two.txt'
           and item['decision'] == 'allow' and item['automatic']
           for item in decisions)
PY
test "$(cat private/one.txt)" = ASK_ONE
test "$(cat private/two.txt)" = ASK_TWO
```

## E. VM and rootfs

Use VM for a stronger boundary, separate guest kernel or OCI rootfs. E01 resembles direct execution, E02/E03 select directory/image sources and E04/E05 add resources/staging.

### S-DOC-024: E01 VM shorthand with host rootfs

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

Preparation: Linux with access to /dev/kvm.

**Semantics**: Guest prints the host workspace absolute path. Result records virtual_machine and networking uses pVisor smoltcp.

**Rationale**: `--vm` selects VM; Linux defaults to host root as guest rootfs, exposing a larger set of readable host files. Use only in a trusted test environment.

**Violation example**: A successful VM returns different cwd or omits vm-smoltcp evidence.

```bash
require_python3
require_linux
require_kvm
case_setup
case_run success <<'CASE_COMMAND'
pvisor --vm -- /bin/pwd > guest-cwd.txt
CASE_COMMAND

test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
bundle_expect run.executor.kind virtual_machine
bundle_expect run.executor.isolation virtual_machine
bundle_expect network.interception.driver vm-smoltcp
bundle_expect network.interception.strength non-bypassable
bundle_expect safety.network_non_bypassable true
```

### S-DOC-025: E02 Explicit VM and directory rootfs

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

Preparation: Linux with access to /dev/kvm. Prepare a Linux rootfs and set PVISOR_CASE_ROOTFS for the script.

**Semantics**: VM succeeds with guest cwd equal to host workspace path.

**Rationale**: Supply an existing Linux rootfs with executable /bin/pwd and dependencies.

**Violation example**: Directory-rootfs VM returns cwd outside the host workspace.

```bash
require_python3
require_linux
require_kvm
require_rootfs
case_setup
case_run success <<'CASE_COMMAND'
pvisor --executor vm --rootfs "$CASE_ROOTFS" -- /bin/pwd > guest-cwd.txt
CASE_COMMAND

test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
bundle_expect run.executor.isolation virtual_machine
```

### S-DOC-026: E03 Image rootfs

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

Preparation: Linux with access to /dev/kvm. Set PVISOR_CASE_IMAGE for the script.

**Semantics**: Image preparation starts VM; guest cwd equals host workspace path.

**Rationale**: Prepare rootfs from OCI without Docker/Podman daemon. Replace the image= placeholder with an accessible reference.

**Violation example**: Image VM returns wrong cwd or lacks virtual_machine result.

```bash
require_python3
require_linux
require_kvm
require_image
case_setup
case_run success <<'CASE_COMMAND'
pvisor --vm --rootfs "image=$CASE_IMAGE" -- /bin/pwd > guest-cwd.txt
CASE_COMMAND

test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
bundle_expect run.executor.isolation virtual_machine
```

### S-DOC-027: E04 VM resources

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

Preparation: Linux with access to /dev/kvm. Prepare a Linux rootfs and set PVISOR_CASE_ROOTFS for the script.

**Semantics**: VM succeeds and memory request is 2147483648 bytes. This case does not assert CPU count.

**Rationale**: Use existing rootfs with 2GiB memory and two vCPUs. Linux static builds embed the kernel.

**Violation example**: Record a different memory value.

```bash
require_python3
require_linux
require_kvm
require_rootfs
case_setup
case_run success <<'CASE_COMMAND'
pvisor --vm \
  --rootfs "$CASE_ROOTFS" \
  --memory 2GiB \
  --cpu 2 \
  -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect run.executor.isolation virtual_machine
bundle_expect resources.requested.memory_bytes 2147483648
```

### S-DOC-028: E05 VM workspace and whole-rootfs stage

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

Preparation: Linux with access to /dev/kvm. Set PVISOR_CASE_IMAGE for the script.

**Semantics**: Guest cwd is preserved and stage remains at vm-stage. Only pwd runs; this checks paths/stage creation, not write capture.

**Rationale**: Add persistent stage to an image VM. Workspace path defaults to host cwd.

**Violation example**: Stage path or guest cwd differs from the request.

```bash
require_python3
require_linux
require_kvm
require_image
case_setup
case_run success <<'CASE_COMMAND'
pvisor --vm \
  --rootfs "image=$CASE_IMAGE" \
  --stage "$CASE_ROOT/vm-stage" \
  -- /bin/pwd > guest-cwd.txt
CASE_COMMAND

test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
bundle_expect filesystem.state staged
record_expect storage "$PVISOR_CASE_ROOT/vm-stage"
```

### S-DOC-029: E06 Reject conflicting executors

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

**Semantics**: Normalization fails and explains that --vm conflicts with a non-VM executor.

**Rationale**: Combine --vm with --executor host to verify rejection.

**Violation example**: Ignore the conflict and launch the workload.

```bash
require_python3
case_setup
case_run nonzero <<'CASE_COMMAND'
pvisor --vm --executor host --rootfs host -- "$CASE_TRUE"
CASE_COMMAND

stdout_has "--vm cannot be combined with a non-vm --executor"
```

## F. Native OCI containers

Use these when an OCI runtime should launch directly. F01 is minimal; F02 changes networking; F03 selects host rootfs; F04 combines advanced settings.

### S-DOC-030: F01 Minimal image container

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Launch native OCI containers directly with runc/crun.

Preparation: Working OCI runtime; set PVISOR_CASE_CONTAINER_IMAGE for the script.

**Semantics**: Container executes /bin/true, succeeds and records isolation type container.

**Rationale**: --container-image selects both container executor and image source.

**Violation example**: Minimal container exits nonzero or records a different executor.

```bash
require_python3
require_container
case_setup
case_run success <<'CASE_COMMAND'
pvisor --container-runtime "$CASE_CONTAINER_RUNTIME" --container-image "$CASE_CONTAINER_IMAGE" -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect run.executor.kind container
bundle_expect run.state completed
bundle_expect run.exit_code 0
```

### S-DOC-031: F02 Container rootfs and isolated network

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Launch native OCI containers directly with runc/crun.

Preparation: Working OCI runtime; set PVISOR_CASE_CONTAINER_IMAGE for the script.

**Semantics**: /bin/true succeeds. It sends no network requests, so assertions check launch rather than resistance to network bypass.

**Rationale**: Add --container-network none to F01: a separate namespace without external connectivity.

**Violation example**: Network isolation prevents simple container startup/completion.

```bash
require_python3
require_container
case_setup
case_run success <<'CASE_COMMAND'
pvisor --container-runtime "$CASE_CONTAINER_RUNTIME" --container-image "$CASE_CONTAINER_IMAGE" \
  --container-network none \
  -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect run.executor.kind container
bundle_expect run.state completed
```

### S-DOC-032: F03 OCI bundle with host rootfs

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Launch native OCI containers directly with runc/crun.

Preparation: Linux with a working OCI runtime and rootless-container user namespaces.

**Semantics**: Execute with a selected directory as rootfs without registry configuration. Use a dedicated test rootfs, not host /, as the test directory.

**Rationale**: Without an image, use host / as read-only lower. pVisor constructs a separate synthetic rootfs and maps standard host directories read-only.

**Violation example**: Host-rootfs OCI bundle fails to complete as container.

```bash
require_python3
require_container_runtime
require_linux
case_setup
case_run success <<'CASE_COMMAND'
pvisor --container-runtime "$CASE_CONTAINER_RUNTIME" --executor container \
  --rootfs host \
  --container-network none \
  -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect run.executor.kind container
bundle_expect run.state completed
```

### S-DOC-033: F04 Explicit runtime and advanced container options

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Launch native OCI containers directly with runc/crun.

Preparation: Working OCI runtime; set PVISOR_CASE_CONTAINER_IMAGE for the script.

**Semantics**: Container succeeds. read_only=false makes the bind writable despite read-only rootfs; --container-workdir is fallback only when Run lacks cwd. This case checks combined launch, not user identity/read-write behavior separately.

**Rationale**: Select runc and a pVisor build, platform, uid/gid, workdir, read-only rootfs and a host bind at /workspace.

**Violation example**: Accepted advanced options fail container execution or record another executor.

```bash
require_python3
require_container
require_runc
case_setup
case_run success <<'CASE_COMMAND'
pvisor --executor container \
  --container-runtime runc \
  --rootfs "image=$CASE_CONTAINER_IMAGE" \
  --container-pvisor-binary "$SUBJECT_BIN" \
  --container-platform linux/amd64 \
  --container-network none \
  --container-workdir /workspace \
  --container-user 1000:1000 \
  --container-read-only-rootfs \
  --container-mount "source=\"$WS\",target=\"/workspace\",read_only=false" \
  -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect run.executor.kind container
bundle_expect run.state completed
```

## G. OverlayNet

These cases concern networking only. Proxy suits cooperative access needing host Gateway; VM auto and host deny-all suit stronger boundaries. With `--ask`, unmatched proxy targets pause for permission: `1` grants this target, `2` grants hostname/subdomains on the same port/transport. IPs have no domain option. `d` denies. As with files, choose saved scope with s/w/u. Explicit denials do not prompt; direct sockets bypassing the proxy do not trigger this audit.

### S-DOC-034: G01 Enable default proxy

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Configure egress, proxy access or network denial.

**Semantics**: Records show explicit-proxy with cooperative strength. Only proxy traffic is constrained; this does not prove direct sockets are blocked.

**Rationale**: --overlaynet without a value enables default proxy mode.

**Violation example**: Omit explicit-proxy/cooperative records or capture artifacts.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor --overlaynet -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect network.interception.driver explicit-proxy
bundle_expect network.interception.strength cooperative
bundle_contains artifacts capture
```

### S-DOC-035: G02 Custom proxy listener

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Configure egress, proxy access or network denial.

**Semantics**: Recorded listen address matches the request and driver is explicit-proxy.

**Rationale**: The listen option enables host proxy. For manual execution keep port 18080 free; scripts substitute a free port.

**Violation example**: Listen on a different loopback address.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor --overlaynet-listen "$CASE_PROXY_LISTEN" -- "$CASE_TRUE"
CASE_COMMAND

record_expect overlaynet_listen "$CASE_PROXY_LISTEN"
bundle_expect network.interception.driver explicit-proxy
```

### S-DOC-036: G03 Combine allow, deny and bandwidth

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Configure egress, proxy access or network denial.

**Semantics**: Records show allowlist, allow api.example.com:443, deny 10.0.0.0/8, and convert 1mbps to 125000 bytes/second. No requests are made.

**Rationale**: Combine allowed destinations, denied subnet and target rate limits; only proxy traffic follows them.

**Violation example**: Lose or rewrite host/port/rate values.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor --overlaynet-allow api.example.com:443 \
  --overlaynet-deny 10.0.0.0/8 \
  --overlaynet-limit api.example.com=1mbps \
  -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect network.policy.mode allowlist
bundle_expect network.policy.rules.0.host api.example.com
bundle_expect network.policy.rules.0.ports.0 443
bundle_expect network.policy.deny_rules.0.host 10.0.0.0/8
bundle_expect network.policy.limits.0.host api.example.com
bundle_expect network.policy.limits.0.bytes_per_second 125000
```

### S-DOC-037: G04 Deny-all cannot be bypassed through environment

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Configure egress, proxy access or network denial.

Preparation: curl installed.

**Semantics**: The command fails and records no-network/non-bypassable. An unavailable external network also makes curl fail, so this case alone cannot prove isolation effectiveness.

**Rationale**: Clear common proxy variables and try external access under deny-all. Linux uses network namespace; macOS Seatbelt; curl deliberately makes a request.

**Violation example**: curl reaches external networking after clearing variables, or artifacts omit mandatory boundary.

```bash
require_python3
require_curl
case_setup
case_run nonzero <<'CASE_COMMAND'
pvisor --overlaynet-deny-all -- /bin/sh -c \
  'unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy; curl --max-time 2 https://example.com'
CASE_COMMAND

bundle_expect network.policy.mode no-network
bundle_expect safety.network_non_bypassable true
bundle_expect run.state failed
```

### S-DOC-038: G05 VM OverlayNet auto

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Configure egress, proxy access or network denial.

Preparation: Linux with access to /dev/kvm. Prepare a Linux rootfs and set PVISOR_CASE_ROOTFS for the script.

**Semantics**: Records show vm-smoltcp/non-bypassable rather than cooperative host proxy.

**Rationale**: Verify VM default auto routes traffic through smoltcp.

**Violation example**: Record VM networking as cooperative proxy.

```bash
require_python3
require_linux
require_kvm
require_rootfs
case_setup
case_run success <<'CASE_COMMAND'
pvisor --vm --rootfs "$CASE_ROOTFS" -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect network.interception.driver vm-smoltcp
bundle_expect network.interception.strength non-bypassable
```

### S-DOC-039: G06 Reject policy when OverlayNet is off

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Configure egress, proxy access or network denial.

**Semantics**: Fail before launch with an error that policy requires auto or proxy. To disable OverlayNet, omit allow/deny/limit.

**Rationale**: Check disabled OverlayNet cannot accept policy settings.

**Violation example**: Silently accept allow policy while off.

```bash
require_python3
case_setup
case_run nonzero <<'CASE_COMMAND'
pvisor --overlaynet off --overlaynet-allow example.com:443 -- "$CASE_TRUE"
CASE_COMMAND

stdout_has "OverlayNet policy options require --overlaynet auto or proxy"
```

### S-DOC-040: G07 Review a specific denied proxy target

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Preparation: curl installed.

**Semantics**: curl fails; Network access observations in status --review identify denied blocked.example:80. Host proxy is cooperative; this proves interception only for the proxied request.

**Rationale**: Request an explicitly denied domain through the injected proxy and verify target/count rather than generic network failures. Policy rejects before requiring a real reachable domain.

**Violation example**: Fail without recording the specific denied target/count.

```bash
require_python3
require_curl
case_setup
case_run nonzero <<'CASE_COMMAND'
pvisor --overlaynet-deny blocked.example -- /bin/sh -c \
  'curl --fail --silent --show-error --noproxy "" -x "$http_proxy" --max-time 2 http://blocked.example/'
CASE_COMMAND

bundle_contains network.intercepted.targets 'HTTP blocked.example:80'
bundle_contains network.intercepted.targets '"denied": 1'
```

## H. Gateway and recording

Use these for audit, model routing or replay. H01 configures Gateway; H02 records JSONL Events.

### S-DOC-041: H01 Combined Gateway capture

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Connect Gateway, route models or record trajectories.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Gateway/stage are created. The upstream is a placeholder and /bin/true makes no model calls, so conversation contents are not tested. Management listen and run-record gateway_listen are different service addresses.

**Rationale**: Configure routes, management port, full capture, session headers, diagnostics and Markdown projection with retained stage.

**Violation example**: Fail Gateway/stage creation or record an invalid loopback gateway_listen.

```bash
require_python3
require_stage
case_setup
case_run success <<'CASE_COMMAND'
pvisor \
  --stage "$CASE_ROOT/gateway-stage" \
  --gateway-mode capture \
  --gateway-admin-listen "$CASE_GATEWAY_LISTEN" \
  --gateway-level full \
  --gateway-session-header X-Session-ID \
  --gateway-debug \
  --gateway-stream-markdown \
  --gateway-route 'name="default",upstream="https://example.com/v1"' \
  -- "$CASE_TRUE"
CASE_COMMAND

record_get gateway_listen | grep -Eq '^127\.0\.0\.1:[0-9]+$'
bundle_expect network.interception.driver explicit-proxy
bundle_expect filesystem.state staged
```

### S-DOC-042: H02 JSON recording

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Connect Gateway, route models or record trajectories.

**Semantics**: Selected file is nonempty and first line resembles a JSON object. Each line should be an Event; this case checks only creation and first-line shape.

**Rationale**: Record Run Events as JSONL.

**Violation example**: Create an empty file or a non-object first line.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor --record-destination "$CASE_ROOT/events.jsonl" -- "$CASE_TRUE"
CASE_COMMAND

test -s "$PVISOR_CASE_ROOT/events.jsonl"
head -n 1 "$PVISOR_CASE_ROOT/events.jsonl" | grep -q '^{'
```

## I. Specs and control plane

Use these for automation/control planes or file-based RunSpec handoff. I01 uses TOML, I02 JSON delegation and I03 an extensionless file.

### S-DOC-043: I01 TOML config

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Execute TOML/JSON configuration or RunSpec through a control plane.

**Semantics**: Command comes from the file without repeating it on CLI; Run completes normally.

**Rationale**: Create pvisor.toml with command = ["/bin/true"] under [run], then use --config; the script supplies it.

**Violation example**: Ignore file command or return wrong name/terminal state.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor --config ./pvisor.toml
CASE_COMMAND

bundle_expect run.state completed
bundle_expect run.agent true
```

### S-DOC-044: I02 JSON RunSpec

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Execute TOML/JSON configuration or RunSpec through a control plane.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Name is case-i02 and run-result.json is nonempty. Delegation currently supports host only and does not apply ordinary Job rootless safe profiles; this is not an isolation-mode example.

**Rationale**: Execute prepared JSON RunSpec and atomically write its result. Manually supply run_id, agent and process invocation; fixtures use /bin/true named case-i02.

**Violation example**: Omit the result or misrepresent delegation as an isolated ordinary Job.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor --spec ./run-spec.json --result-file ./run-result.json --stage ./delegated-stage
CASE_COMMAND

bundle_expect run.agent case-i02
bundle_expect run.executor.isolation host_process
test -s run-result.json
```

### S-DOC-045: I03 Extensionless spec

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Execute TOML/JSON configuration or RunSpec through a control plane.

**Semantics**: --config reads TOML and completes without a .toml suffix.

**Rationale**: Save I01 TOML as config-without-extension; the script supplies it.

**Violation example**: Reject the same TOML due to its filename.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
pvisor --config ./config-without-extension
CASE_COMMAND

bundle_expect run.state completed
```

## J. Combined configurations

These regress multiple capabilities: J01 host, J02 VM, J03 container. Short test commands cannot replace real agent workload acceptance. Diagnose failures by returning to A–I cases.

### S-DOC-046: J01 Host + persistent stage + deny-all + capture + limits

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Validate combined capabilities before deployment.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Original workspace is unchanged; stage includes result.txt; result retains stdout/resource requests; selected JSONL records Events; networking is denied.

**Rationale**: Combine host stage, deny-all, output capture, JSON Events and limits while writing a result in the isolated view.

**Violation example**: Leak result.txt into the workspace or omit trajectory/resource/network evidence.

```bash
require_python3
require_stage
case_setup
case_run success <<'CASE_COMMAND'
pvisor --name host-full \
  --stage "$CASE_ROOT/host-full" \
  --overlaynet-deny-all \
  --stdio capture \
  --record-destination "$CASE_ROOT/host-full/trajectory/events.jsonl" \
  --memory 512MiB \
  --max-processes 64 \
  --overlayfs-max-size 2GiB \
  --max-cpu-time 30s \
  -- /bin/sh -c 'pwd; printf changed > result.txt'
CASE_COMMAND

bundle_expect run.agent host-full
test "$(bundle_get run.output.stdout)" = "$(record_get overlay.merged_dir)"
bundle_expect network.policy.mode no-network
bundle_expect safety.network_non_bypassable true
bundle_expect safety.filesystem_changes_staged true
bundle_contains filesystem.changes result.txt
bundle_expect resources.requested.memory_bytes 536870912
bundle_expect resources.requested.processes 64
bundle_expect resources.requested.cpu_time_ms 30000
test ! -e result.txt
test -s "$PVISOR_CASE_ROOT/host-full/trajectory/events.jsonl"
```

### S-DOC-047: J02 VM + image rootfs + stage + OverlayNet + Gateway

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Validate combined capabilities before deployment.

Preparation: Linux with access to /dev/kvm. Set PVISOR_CASE_IMAGE for the script. PVISOR_CASE_AGENT must execute inside the guest.

**Semantics**: VM uses requested memory and retains stage/trajectory directory. Conversation output depends on actual Gateway calls; current assertions do not inspect conversation contents.

**Rationale**: Run a real agent in an image with its dependencies, retaining stage, smoltcp, Gateway and JSONL. Replace the placeholder upstream with a real service.

**Violation example**: Lose stage/trajectory or 4GiB memory request.

```bash
require_python3
require_linux
require_kvm
require_image
require_agent
case_setup
case_run success <<'CASE_COMMAND'
pvisor --name vm-full \
  --vm \
  --rootfs "image=$CASE_IMAGE" \
  --stage "$CASE_ROOT/vm-full" \
  --overlaynet auto \
  --gateway-mode capture \
  --gateway-level dialogue \
  --gateway-route 'name="default",upstream="https://example.com/v1"' \
  --record-destination "$CASE_ROOT/vm-full/trajectory" \
  --memory 4GiB \
  --cpu 4 \
  -- "$CASE_AGENT"
CASE_COMMAND

bundle_expect run.agent vm-full
bundle_expect run.executor.isolation virtual_machine
bundle_expect network.interception.driver vm-smoltcp
bundle_expect filesystem.state staged
bundle_expect resources.requested.memory_bytes 4294967296
test -d "$PVISOR_CASE_ROOT/vm-full/trajectory"
```

### S-DOC-048: J03 Container + stage + read-only root + no network

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Suggested use: Validate combined capabilities before deployment.

Preparation: Working OCI runtime; set PVISOR_CASE_CONTAINER_IMAGE for the script.

**Semantics**: Container succeeds and leaves a stage. /bin/true generates no changes or meaningful stdout; this does not test stage writes or network blocking.

**Rationale**: Combine persistent stage, read-only rootfs, isolated network and stdout capture. Rootfs read-only and writable stage are different settings.

**Violation example**: Fail container completion/stage retention.

```bash
require_python3
require_container
case_setup
case_run success <<'CASE_COMMAND'
pvisor --container-runtime "$CASE_CONTAINER_RUNTIME" --name container-full \
  --container-image "$CASE_CONTAINER_IMAGE" \
  --container-read-only-rootfs \
  --container-network none \
  --stage "$CASE_ROOT/container-full" \
  --stdio capture \
  -- "$CASE_TRUE"
CASE_COMMAND

bundle_expect run.agent container-full
bundle_expect run.executor.kind container
bundle_expect run.state completed
bundle_expect filesystem.state staged
```

## K. Job review and lifecycle

Job is the user-facing object. These commands use its stage path directly as selector without another job subcommand.

### S-DOC-049: K01 Review and inspect staged files read-only

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Preparation: Linux user/mount namespaces available.

**Semantics**: Review lists one file; the read-only view reads staged, rejects writes and leaves the original workspace unchanged.

**Rationale**: Use status --review then inspect staged content and verify inspect cannot write.

**Violation example**: Allow inspect writes or put note.txt in the original workspace.

```bash
require_python3
require_rootless
case_setup
case_run success <<'CASE_COMMAND'
pvisor --stage "$CASE_ROOT/review-stage" -- /bin/sh -c 'printf staged > note.txt'
pvisor status --review --json "$CASE_ROOT/review-stage" > status.json
pvisor inspect "$CASE_ROOT/review-stage" -- /bin/cat note.txt
if pvisor inspect "$CASE_ROOT/review-stage" -- /bin/sh -c 'printf changed > note.txt'; then
  exit 1
fi
CASE_COMMAND

stdout_has staged
test ! -e note.txt
python3 -c 'import json; d=json.load(open("status.json")); assert d["filesystem"]["changed_files"] == 1'
```

### S-DOC-050: K02 Selective apply then discard remaining changes

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Preparation: Linux user/mount namespaces available.

**Semantics**: Only one.txt reaches original workspace; two.txt never reaches lower.

**Rationale**: Apply one.txt, retain two.txt pending a decision, then explicitly drop remaining changes.

**Violation example**: Apply two.txt alongside one.txt or fail to discard the remaining stage.

```bash
require_python3
require_rootless
case_setup
case_run success <<'CASE_COMMAND'
pvisor --stage "$CASE_ROOT/partial-stage" -- /bin/sh -c \
  'printf one > one.txt; printf two > two.txt'
pvisor apply "$CASE_ROOT/partial-stage" --path one.txt
pvisor drop "$CASE_ROOT/partial-stage"
CASE_COMMAND

test "$(cat one.txt)" = one
test ! -e two.txt
record_expect overlay.state discarded "$PVISOR_CASE_ROOT/partial-stage"
```

### S-DOC-051: K03 Fork a stopped Job

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Preparation: Linux user/mount namespaces available.

**Semantics**: Child reads inherited; neither Job writes changes directly to original workspace.

**Rationale**: Start a child from the source staged view, reading its changes and creating independent ones.

**Violation example**: Lose source changes or leak child writes into original workspace.

```bash
require_python3
require_rootless
case_setup
case_run success <<'CASE_COMMAND'
pvisor --stage "$CASE_ROOT/source-stage" -- /bin/sh -c 'printf inherited > inherited.txt'
pvisor fork "$CASE_ROOT/source-stage" -- /bin/sh -c \
  'cat inherited.txt; printf child > child.txt' > child.out
CASE_COMMAND

test "$(cat child.out)" = inherited
test ! -e inherited.txt
test ! -e child.txt
bundle_contains filesystem.changes inherited.txt "$PVISOR_CASE_ROOT/source-stage"
bundle_contains filesystem.changes child.txt "$PVISOR_CASE_RECORDS"
```

### S-DOC-052: K04 Terminate a running Job

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Preparation: Linux user/mount namespaces available.

**Semantics**: Job exits before sleep completes; status --json reports cancelled and no live state.

**Rationale**: Background a long Job and request normal termination by stage path.

**Violation example**: Remain live after kill or report a different terminal state.

```bash
require_python3
require_rootless
case_setup
case_run success <<'CASE_COMMAND'
pvisor --stage "$CASE_ROOT/live-stage" -- /bin/sleep 30 > live.log 2>&1 &
job_pid=$!
for ((attempt=0; attempt<100; attempt++)); do
  test -f "$CASE_ROOT/live-stage/run.json" && break
  sleep 0.05
done
pvisor kill "$CASE_ROOT/live-stage"
if wait "$job_pid"; then exit 1; fi
pvisor status --json "$CASE_ROOT/live-stage" > stopped.json
CASE_COMMAND

python3 -c 'import json; d=json.load(open("stopped.json")); assert d["run"]["state"] == "cancelled" and d["live"] is False'
```

## L. Removed environment commands

The env feature was removed. S-DOC-053/S-DOC-054 are registered retired and never reused.

## M. Replay and interactive terminal

### S-DOC-055: M01 Prepare a replay prefix offline

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

**Semantics**: Output phase is prepared; historical commands did not create marker.

**Rationale**: Use a minimal native mini-swe-agent trajectory with replay --prepare-only. It parses the prefix without launching an agent or executing historical tools.

**Violation example**: Execute history/create marker or report replayed calls in prepare-only.

```bash
require_python3
case_setup
case_run success <<'CASE_COMMAND'
cat > trajectory.json <<'JSON'
{"trajectory_format":"mini-swe-agent-1.1","info":{"mini_version":"2.4.6"},"messages":[{"role":"assistant","content":"historical action","extra":{"response":{},"actions":[{"tool_call_id":"call-1","command":"printf should-not-run > marker"}]}},{"role":"tool","content":"old observation","extra":{"returncode":0}}]}
JSON
pvisor replay --agent mini-swe-agent --trajectory ./trajectory.json \
  --after-step 1 --prepare-only \
  --state-dir "$CASE_ROOT/replay-state" \
  --output-dir "$CASE_ROOT/replay-output" > prepared.json
CASE_COMMAND

test ! -e marker
python3 -c 'import json; d=json.load(open("prepared.json")); assert d["phase"] == "prepared" and d["replayed_tool_calls"] == 0'
```

### S-DOC-056: M02 TUI preserves output and opens Log

<!-- semantic-case: vocab=core.sh,cases.sh,pvisor.sh -->

Preparation: Linux and Python 3.

**Semantics**: Child exits normally; screen stream contains command output, footer guidance and Log panel.

**Rationale**: Use a pseudo-terminal to run --tui and check output, footer keys and Ctrl-] then l to open floating Log. Test scripts provide the terminal.

**Violation example**: Lose output or fail to show footer/Log after Ctrl-].

```bash
require_python3
require_linux
case_setup
case_run success <<'CASE_COMMAND'
python3 - <<'PY'
import fcntl, os, pty, select, struct, subprocess, termios, time

master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
env = os.environ.copy()
env['TERM'] = 'xterm-256color'
child = subprocess.Popen(
    ['pvisor', '--tui', '--', '/bin/sh', '-c', 'printf TUI_READY; sleep 1.5'],
    stdin=slave, stdout=slave, stderr=slave, env=env, start_new_session=True,
)
os.close(slave)
screen = bytearray()
sent = False
deadline = time.monotonic() + 8
try:
    while time.monotonic() < deadline:
        ready, _, _ = select.select([master], [], [], 0.1)
        if ready:
            try:
                screen.extend(os.read(master, 65536))
            except OSError:
                break
        if not sent and b'TUI_READY' in screen:
            os.write(master, b'\x1dl')
            sent = True
        if child.poll() is not None and not ready:
            break
    if child.poll() is None:
        child.kill()
    child.wait(timeout=2)
finally:
    os.close(master)
assert child.returncode == 0, child.returncode
assert sent and b'TUI_READY' in screen
assert b'Ctrl-]' in screen and b'pVisor Review' in screen
print('TUI_READY status-bar log-panel')
PY
CASE_COMMAND

stdout_has 'TUI_READY status-bar log-panel'
```
