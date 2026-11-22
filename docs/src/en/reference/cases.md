# pVisor Session scenarios and regression examples

Start with a simple Session, add limits, staging, VM, container and networking, then complete review, branching and trajectory replay. Each case explains purpose, preparation and expected results before executable commands. IDs allow individual regressions.

`run` creates a Session; `status`, `inspect`, `apply`, `drop`, `fork` and `kill` operate on it directly. `replay` creates a Session from a trajectory. Labels such as A01 identify regressions and reports.

## Choose by need

| Need | Start here |
| --- | --- |
| Run a command or verify default writes | A01–A03, A07 |
| Timeout, memory or file limits | A04, B01–B04 |
| Keep, discard or inspect file changes | C01–C06 |
| Compose OverlayFS layers or grant path permissions | D01, D04–D05 |
| Understand default host isolation | D02–D03 |
| VM, host rootfs or OCI images | E01–E06 |
| Native OCI containers | F01–F04 |
| Proxy configuration or network denial | G01–G07 |
| Gateway or trajectory recording | H01–H02 |
| Configuration files or RunSpec | I01–I03 |
| Combine capabilities | J01–J03 |
| Review, selective apply, fork or terminate | K01–K04 |
| Prepare trajectory replay | M01 |

This appendix retains each purpose, semantic claim, violation example and original A01–M01 label. Every scenario links to its single native Scrut check in `tests/spec/<feature>/S-ID.md`. `mise run cases` selects specifications by filename and calls Scrut; it does not extract checks from documentation pages.

## How to use

For manual product commands, prepare a test working directory and ensure `pvisor` is on `PATH`. Full check blocks use runner fixtures/assertion functions; execute them with `mise run cases`.

```bash
mkdir -p /tmp/pvisor-cases/workspace
cd /tmp/pvisor-cases/workspace
```

Run automated checks from the repository root:

```bash
mise run cases -- --suite doc --list
mise run cases -- --subject-bin target/release/pvisor
mise run cases -- --case S-DOC-001,S-DOC-012 --keep
mise run cases
SUBJECT_BIN="$PWD/target/release/pvisor" mise run scrut -- test tests/spec/session/S-DOC-001.md
```

`mise run cases` builds release pVisor, discovers the 52 active DOC specifications described here and [six VM control and RAM backing cases](cases-vm.md) and writes native Scrut JSON to `target/pvisor-case-report.json`. Select by S-DOC ID: A01 is S-DOC-001, C01 is S-DOC-012. Headings retain each original label. Use an existing binary with `mise run cases -- --subject-bin PATH`. L01/L02 and S-DOC-053/S-DOC-054 were removed with `env`; their IDs are retired and never reused.

Each native specification retains the original commands, expected exits and all assertions. Expected nonzero exits must actually occur and satisfy the original assertions; they are not XFAIL. Shared functions in `tests/spec/fixtures/` read this case's Run Bundle, run record and command logs. Configuration, Sessions and fixtures live in independent temporary CASE_ROOT directories. Failures retain their resources; `--keep` also retains successes. Missing prerequisites are still labelled SKIP but return failure, so CI cannot count them as passing coverage.

These specifications and fixtures still await human review; passing checks are not approval. Historical ledgers remain unchanged. Maintainers have not yet decided the digest-review policy for Scrut. `--require-reviewed` explicitly refuses execution rather than bypassing the review requirement.

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


Suggested use: First use: verify commands and Session identity.

**Semantics**: Print the absolute current workspace path and exit successfully. Default executor is host, without staging; networking is ambient when no policy is configured.

**Rationale**: Run in the current directory without explicitly writing `run`. Everything after `--` is the agent command and arguments.

**Violation example**: Exit 0 while printing the parent directory, or start staging by default.

Execution and assertions: [Scrut S-DOC-001](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-001.md).

### S-DOC-002: A02 Explicit run equals the short form


Suggested use: First use: verify commands and Session identity.

**Semantics**: Both output files contain the same current working directory. Each execution has a separate record; Session IDs and times may differ.

**Rationale**: Compare explicit/implicit `run`, saving agent stdout separately.

**Violation example**: Working directory output differs between the two forms.

Execution and assertions: [Scrut S-DOC-002](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-002.md).

### S-DOC-003: A03 Session name and stdio capture


Suggested use: First use: verify commands and Session identity.

**Semantics**: Session name is `smoke`, result stdout is `hello` and is not truncated.

**Rationale**: Name the Run with `--name smoke` and retain output with `--stdio capture`.

**Violation example**: Lose the agent name or mark hello truncated.

Execution and assertions: [Scrut S-DOC-003](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-003.md).

### S-DOC-004: A04 Timeout


Suggested use: First use: verify commands and Session identity.

**Semantics**: pVisor exits nonzero and records failure type `deadline_exceeded`.

**Rationale**: Set a wall-clock timeout. `100ms` is elapsed duration from start, not CPU time; the command deliberately sleeps 10 seconds.

**Violation example**: The timed-out command succeeds or records an ordinary retryable process_exit failure.

Execution and assertions: [Scrut S-DOC-004](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-004.md).

### S-DOC-005: A05 Strict mode rejects best-effort boundaries


Suggested use: First use: verify commands and Session identity.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Current host/container/VM paths reject the request before launching the agent because Subprocess enforcement evidence is missing (`UnsupportedPolicy`). This verifies fail-closed behavior; it does not mean a stronger sandbox is currently ready under `--strict` on any executor.

**Rationale**: Require strict capability checks and deny networking. `--strict` rejects requested capabilities without enforcement evidence.

**Violation example**: Launch despite missing evidence, or reject without naming the missing capability.

Execution and assertions: [Scrut S-DOC-005](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-005.md).

### S-DOC-006: A06 Explicit environment projection


Suggested use: First use: verify commands and Session identity.

**Semantics**: The child sees `TEST_PVISOR_VALUE=visible`; records list the variable without claiming full host environment inheritance.

**Rationale**: Set a variable only for this command and explicitly project it through `--pass-env`.

**Violation example**: Omit the permitted variable or incorrectly claim complete host inheritance.

Execution and assertions: [Scrut S-DOC-006](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-006.md).

### S-DOC-007: A07 Default writes reach the workspace


**Semantics**: After exit, `direct.txt` exists in the original workspace and records contain no OverlayFS stage.

**Rationale**: Verify the ordinary host Session default writable lower without selecting another executor/stage.

**Violation example**: A host write without a staging request does not reach the workspace.

Execution and assertions: [Scrut S-DOC-007](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-007.md).

## B. Resource limits

These cases distinguish requested limits from actual enforcement. B01 inspects complete configuration; B02 actually tries to exceed a file limit.

### S-DOC-008: B01 Combine all resource limits


Suggested use: Control or verify resource limits.

**Semantics**: The command succeeds and all five requested values appear in records alongside effective limits/mechanisms. `/bin/true` does not consume the allowances; this does not test exceeding them.

**Rationale**: Combine memory, process count, CPU time, open file count and single-file size. MiB is binary; `--max-cpu-time` differs from wall-clock timeout.

**Violation example**: Accept options but record incorrect requests, or omit the effective file-size limit/rlimit mechanism.

Execution and assertions: [Scrut S-DOC-008](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/resources/S-DOC-008.md).

### S-DOC-009: B02 File-size limit is enforced


Suggested use: Control or verify resource limits.

**Semantics**: Writing fails; any file left on disk is at most 1024 bytes. The result records a process-exit failure.

**Rationale**: Set 1KiB and use `dd` to execution a 4KiB write.

**Violation example**: dd writes 4KiB successfully or leaves more than 1024 bytes after failure.

Execution and assertions: [Scrut S-DOC-009](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/resources/S-DOC-009.md).

### S-DOC-010: B03 Short memory alias


Suggested use: Control or verify resource limits.

**Semantics**: The command succeeds and requests 268435456 bytes, equal to `--memory 256MiB`.

**Rationale**: Use `--mem`, an alias for `--memory`, with a simple command.

**Violation example**: Parse --mem 256MiB differently from --memory.

Execution and assertions: [Scrut S-DOC-010](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/resources/S-DOC-010.md).

### S-DOC-011: B04 Total stage size limit


Suggested use: Control or verify resource limits.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: The stage is created and retained at the selected path. This verifies option acceptance/directory creation only; artifacts do not currently record the limit, and the case does not fill the stage.

**Rationale**: Request a 1GiB total stage limit, distinct from B02 single-file size.

**Violation example**: The command succeeds without staged filesystem state or with the wrong storage path.

Execution and assertions: [Scrut S-DOC-011](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/resources/S-DOC-011.md).

## C. Staging and whole-rootfs

Use these when an agent should modify files without changing the current workspace. C01 is explicit persistent staging; C02 lets `--safe` select/retain the stage; C03 drops a persistent stage explicitly.

### S-DOC-012: C01 Persistent stage


Suggested use: Isolate file changes, retain a stage or verify whole-rootfs behavior.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Original workspace has no `result.txt`; the change list includes it and the selected stage retains `run-bundle.json` for later inspection.

**Rationale**: Create result.txt in the workspace view and retain its changes in a stage.

**Violation example**: result.txt reaches the original workspace or is omitted from changes.

Execution and assertions: [Scrut S-DOC-012](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-012.md).

### S-DOC-013: C02 Retain the stage by default


Suggested use: Isolate file changes, retain a stage or verify whole-rootfs behavior.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: The command succeeds; the logged storage directory/Bundle remain and the new file is absent from the original workspace.

**Rationale**: Without `--stage`, `--safe` uses persistent Session storage and retains changes after exit.

**Violation example**: The logged Bundle disappears after exit or result.txt reaches the original workspace.

Execution and assertions: [Scrut S-DOC-013](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-013.md).

### S-DOC-014: C03 Explicitly discard a persistent stage


Suggested use: Isolate file changes, retain a stage or verify whole-rootfs behavior.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: The stage directory remains; filesystem state becomes `discarded`; no new file appears in the original workspace.

**Rationale**: Select a persistent stage then discard changes using `pvisor drop` after completion.

**Violation example**: Records stay staged after drop or result.txt reaches the original workspace.

Execution and assertions: [Scrut S-DOC-014](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-014.md).

### S-DOC-015: C04 Preserve existing stage directory contents


Suggested use: Isolate file changes, retain a stage or verify whole-rootfs behavior.

**Semantics**: The command succeeds, retaining existing `user-file` and the new Bundle in the selected directory.

**Rationale**: `--stage PATH` always denotes persistent storage; cleanup must not delete existing user files.

**Violation example**: Cleanup deletes the original user-file.

Execution and assertions: [Scrut S-DOC-015](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-015.md).

### S-DOC-016: C05 Whole-rootfs capture and tmpfs isolation


Suggested use: Isolate file changes, retain a stage or verify whole-rootfs behavior.

Preparation: Linux user/mount namespaces; macOS Seatbelt does not provide this whole-rootfs/tmpfs isolation.

**Semantics**: Workspace changes appear in stage; neither host workspace nor host `/tmp` gets new files. This does not verify persistence of ordinary rootfs paths outside the workspace.

**Rationale**: Compare retained workspace writes with temporary sandbox-directory writes used only for this Run.

**Violation example**: Workspace changes are not staged or sandbox /tmp writes reach host /tmp.

Execution and assertions: [Scrut S-DOC-016](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-016.md).

### S-DOC-017: C06 Safe isolates HOME writes


Preparation: Linux user/mount namespaces available.

**Semantics**: The agent reads its new HOME state; host HOME has no such file; persistent workspace stage remains reviewable.

**Rationale**: Check `--safe` provides a separate HOME copy-on-write view as well as workspace staging. A dedicated test HOME avoids the real user directory.

**Violation example**: Write HOME/state to host HOME or prevent the agent from reading its own write.

Execution and assertions: [Scrut S-DOC-017](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-017.md).

## D. OverlayFS and host security boundaries

D01 covers layer composition; D02/D03 host execution; D04–D05 denial and direct
writes. File rules `deny` and `warn` mean reject and allow with warning; they
append by default. `--mount` mode `read` grants a read-only host share (host
plus `--safe` required), `stage` composes copy-on-write lowers, and `write`
writes directly to host lower. Review staged changes with `review`, accept
selected paths with `apply`, and discard remaining changes with `drop`.

### S-DOC-018: D01 Advanced OverlayFS composition


Suggested use: Inspect OverlayFS views and host security boundaries.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: The recorded target is `view`; lowers are `layer`, `base`, then the Run-owned workspace snapshot, top to bottom. Empty directories test configuration order, not same-name override contents.

**Rationale**: Layer two host directories into the workspace view and choose the agent-visible path. `directory` selects that backend; only explicit apply commits changes.

**Violation example**: Reverse layer/base order or omit the workspace snapshot directory.

Execution and assertions: [Scrut S-DOC-018](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-018.md).

### S-DOC-019: D02 Explicit host executor


Suggested use: Inspect OverlayFS views and host security boundaries.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Linux records `rootless_process`; macOS records `sandboxed_process`; neither may degrade to host process.

**Rationale**: Explicitly choose host and inspect current-platform isolation type.

**Violation example**: Downgrade to host_process while recording isolation success.

Execution and assertions: [Scrut S-DOC-019](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-019.md).

### S-DOC-020: D03 Host stage hides the original workspace path


Suggested use: Inspect OverlayFS views and host security boundaries.

Preparation: Linux user/mount namespaces; macOS lacks this procfs/mount-namespace path-hiding behavior.

**Semantics**: cwd points to stage merged, and output omits the original workspace path. This checks displayed paths only, not denial of all original-path/inherited-FD access.

**Rationale**: Inspect child cwd/procfs under staging, saving three command outputs to views.txt.

**Violation example**: procfs cwd exposes the original workspace path.

Execution and assertions: [Scrut S-DOC-020](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-020.md).

### S-DOC-021: D04 Deny sensitive file reads explicitly


Preparation: Linux user/mount namespaces available.

**Semantics**: Read fails, the deny rule is recorded, and host content remains unchanged.

**Rationale**: Use `--access PATH-GLOB:deny` for matching workspace files.

**Violation example**: Allow private/token or omit the denied target from review.

Execution and assertions: [Scrut S-DOC-021](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-021.md).

### S-DOC-022: D05 Direct writes to an explicit share


Preparation: Linux user/mount namespaces available.

**Semantics**: Shared directory file `out` is written directly to host lower without apply.

**Rationale**: Grant a writable external host directory with `--mount SOURCE:write`.

**Violation example**: An explicit write share fails to receive mounted content.

Execution and assertions: [Scrut S-DOC-022](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-022.md).

## E. VM and rootfs

Use VM for a stronger boundary, separate guest kernel or OCI rootfs. E01 resembles direct execution, E02/E03 select directory/image sources and E04/E05 add resources/staging.

### S-DOC-024: E01 VM shorthand with host rootfs


Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

Preparation: Linux with access to /dev/kvm.

**Semantics**: Guest prints the host workspace absolute path. Result records virtual_machine and networking uses pVisor smoltcp.

**Rationale**: `--vm` selects VM; Linux defaults to host root as guest rootfs, exposing a larger set of readable host files. Use only in a trusted test environment.

**Violation example**: A successful VM returns different cwd or omits vm-smoltcp evidence.

Execution and assertions: [Scrut S-DOC-024](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-024.md).

### S-DOC-025: E02 Explicit VM and directory rootfs


Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

Preparation: Linux with access to /dev/kvm. Prepare a Linux rootfs and set PVISOR_CASE_ROOTFS for the script.

**Semantics**: VM succeeds with guest cwd equal to host workspace path.

**Rationale**: Supply an existing Linux rootfs with executable /bin/pwd and dependencies.

**Violation example**: Directory-rootfs VM returns cwd outside the host workspace.

Execution and assertions: [Scrut S-DOC-025](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-025.md).

### S-DOC-026: E03 Image rootfs


Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

Preparation: Linux with access to /dev/kvm. Set PVISOR_CASE_IMAGE for the script.

**Semantics**: Image preparation starts VM; guest cwd equals host workspace path.

**Rationale**: Prepare rootfs from OCI without Docker/Podman daemon. Replace the image= placeholder with an accessible reference.

**Violation example**: Image VM returns wrong cwd or lacks virtual_machine result.

Execution and assertions: [Scrut S-DOC-026](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-026.md).

### S-DOC-027: E04 VM resources


Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

Preparation: Linux with access to /dev/kvm. Prepare a Linux rootfs and set PVISOR_CASE_ROOTFS for the script.

**Semantics**: VM succeeds and memory request is 2147483648 bytes. This case does not assert CPU count.

**Rationale**: Use existing rootfs with 2GiB memory and two vCPUs. Linux static builds embed the kernel.

**Violation example**: Record a different memory value.

Execution and assertions: [Scrut S-DOC-027](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-027.md).

### S-DOC-028: E05 VM workspace and whole-rootfs stage


Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

Preparation: Linux with access to /dev/kvm. Set PVISOR_CASE_IMAGE for the script.

**Semantics**: Guest cwd is preserved and stage remains at vm-stage. Only pwd runs; this checks paths/stage creation, not write capture.

**Rationale**: Add persistent stage to an image VM. Workspace path defaults to host cwd.

**Violation example**: Stage path or guest cwd differs from the request.

Execution and assertions: [Scrut S-DOC-028](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-028.md).

### S-DOC-029: E06 Reject conflicting executors


Suggested use: Use a guest kernel, separate rootfs or stronger isolation.

**Semantics**: Normalization fails and explains that --vm conflicts with a non-VM executor.

**Rationale**: Combine --vm with --executor host to verify rejection.

**Violation example**: Ignore the conflict and launch the workload.

Execution and assertions: [Scrut S-DOC-029](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-029.md).

## F. Native OCI containers

Use these when an OCI runtime should launch directly. F01 is minimal; F02 changes networking; F03 selects host rootfs; F04 combines advanced settings.

These examples use native OCI bundles and runc/crun without a Docker/Podman daemon. F01–F03 use the fixture-selected runtime; F04 explicitly chooses runc. Provision an image or rootfs compatible with the native architecture. The runtime starts the workload directly.

F04's explicit `--container-platform linux/amd64` assumes Linux x86_64. This option accepts a matching native assertion and rejects cross-architecture values, including with prepared rootfs; host/VM configurations reject it. It is not an emulation, executable auto-discovery or download selector. The existing case checks combined launch, not a platform-rejection matrix. See [container setup](../guides/executors/container.md) and [configuration](config.md#settings).

### S-DOC-030: F01 Minimal image container


Suggested use: Launch native OCI containers directly with runc/crun.

Preparation: Working OCI runtime; set PVISOR_CASE_CONTAINER_IMAGE for the script.

**Semantics**: Container executes /bin/true, succeeds and records isolation type container.

**Rationale**: --container-image selects both container executor and image source.

**Violation example**: Minimal container exits nonzero or records a different executor.

Execution and assertions: [Scrut S-DOC-030](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/container/S-DOC-030.md).

### S-DOC-031: F02 Container rootfs and isolated network


Suggested use: Launch native OCI containers directly with runc/crun.

Preparation: Working OCI runtime; set PVISOR_CASE_CONTAINER_IMAGE for the script.

**Semantics**: /bin/true succeeds. It sends no network requests, so assertions check launch rather than resistance to network bypass.

**Rationale**: Add --container-network none to F01: a separate namespace without external connectivity.

**Violation example**: Network isolation prevents simple container startup/completion.

Execution and assertions: [Scrut S-DOC-031](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/container/S-DOC-031.md).

### S-DOC-032: F03 OCI bundle with host rootfs


Suggested use: Launch native OCI containers directly with runc/crun.

Preparation: Linux with a working OCI runtime and rootless-container user namespaces.

**Semantics**: Execute with a selected directory as rootfs without registry configuration. Use a dedicated test rootfs, not host /, as the test directory.

**Rationale**: Without an image, use host / as read-only lower. pVisor constructs a separate synthetic rootfs and maps standard host directories read-only.

**Violation example**: Host-rootfs OCI bundle fails to complete as container.

Execution and assertions: [Scrut S-DOC-032](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/container/S-DOC-032.md).

### S-DOC-033: F04 Explicit runtime and advanced container options


Suggested use: Launch native OCI containers directly with runc/crun.

Preparation: Working OCI runtime; set PVISOR_CASE_CONTAINER_IMAGE for the script.

**Semantics**: Container succeeds. read_only=false makes the bind writable despite read-only rootfs; --container-workdir is fallback only when Run lacks cwd. This case checks combined launch, not user identity/read-write behavior separately.

**Rationale**: Select runc, platform, uid/gid, workdir, read-only rootfs and a host bind at /workspace. The fixture also passes the legacy binary option, which is accepted but ignored.

**Violation example**: Accepted advanced options fail container execution or record another executor.

Execution and assertions: [Scrut S-DOC-033](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/container/S-DOC-033.md).

## G. OverlayNet

These cases concern networking only. Proxy suits cooperative access needing host
Gateway; VM auto and host deny-all suit stronger boundaries. Configure static
network rules before launch; direct sockets can bypass a cooperative proxy.

### S-DOC-034: G01 Enable default proxy


Suggested use: Configure egress, proxy access or network denial.

**Semantics**: Records show explicit-proxy with cooperative strength. Only proxy traffic is constrained; this does not prove direct sockets are blocked.

**Rationale**: --overlaynet without a value enables default proxy mode.

**Violation example**: Omit explicit-proxy/cooperative records or capture artifacts.

Execution and assertions: [Scrut S-DOC-034](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-034.md).

### S-DOC-035: G02 Custom proxy listener


Suggested use: Configure egress, proxy access or network denial.

**Semantics**: Recorded listen address matches the request and driver is explicit-proxy.

**Rationale**: The listen option enables host proxy. For manual execution keep port 18080 free; scripts substitute a free port.

**Violation example**: Listen on a different loopback address.

Execution and assertions: [Scrut S-DOC-035](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-035.md).

### S-DOC-036: G03 Combine allow, deny and bandwidth


Suggested use: Configure egress, proxy access or network denial.

**Semantics**: Records show allowlist, allow api.example.com:443, deny 10.0.0.0/8, and convert 1mbps to 125000 bytes/second. No requests are made.

**Rationale**: Combine allowed destinations, denied subnet and target rate limits; only proxy traffic follows them.

**Violation example**: Lose or rewrite host/port/rate values.

Execution and assertions: [Scrut S-DOC-036](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-036.md).

### S-DOC-037: G04 Deny-all cannot be bypassed through environment


Suggested use: Configure egress, proxy access or network denial.

Preparation: curl installed.

**Semantics**: The command fails and records no-network/non-bypassable. An unavailable external network also makes curl fail, so this case alone cannot prove isolation effectiveness.

**Rationale**: Clear common proxy variables and try external access under deny-all. Linux uses network namespace; macOS Seatbelt; curl deliberately makes a request.

**Violation example**: curl reaches external networking after clearing variables, or artifacts omit mandatory boundary.

Execution and assertions: [Scrut S-DOC-037](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-037.md).

### S-DOC-038: G05 VM OverlayNet auto


Suggested use: Configure egress, proxy access or network denial.

Preparation: Linux with access to /dev/kvm. Prepare a Linux rootfs and set PVISOR_CASE_ROOTFS for the script.

**Semantics**: Records show vm-smoltcp/non-bypassable rather than cooperative host proxy.

**Rationale**: Verify VM default auto routes traffic through smoltcp.

**Violation example**: Record VM networking as cooperative proxy.

Execution and assertions: [Scrut S-DOC-038](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-038.md).

### S-DOC-039: G06 Reject policy when OverlayNet is off


Suggested use: Configure egress, proxy access or network denial.

**Semantics**: Fail before launch with an error that policy requires auto or proxy. To disable OverlayNet, omit allow/deny/limit.

**Rationale**: Check disabled OverlayNet cannot accept policy settings.

**Violation example**: Silently accept allow policy while off.

Execution and assertions: [Scrut S-DOC-039](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-039.md).

### S-DOC-040: G07 Review a specific denied proxy target


Preparation: curl installed.

**Semantics**: curl fails; Network access observations in status --review identify denied blocked.example:80. Host proxy is cooperative; this proves interception only for the proxied request.

**Rationale**: Request an explicitly denied domain through the injected proxy and verify target/count rather than generic network failures. Policy rejects before requiring a real reachable domain.

**Violation example**: Fail without recording the specific denied target/count.

Execution and assertions: [Scrut S-DOC-040](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-040.md).

## H. Gateway and recording

Use these for audit, model routing or replay. H01 configures Gateway; H02 records JSONL Events.

### S-DOC-041: H01 Combined Gateway capture


Suggested use: Connect Gateway, route models or record trajectories.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Gateway/stage are created. The upstream is a placeholder and /bin/true makes no model calls, so conversation contents are not tested. Management listen and run-record gateway_listen are different service addresses.

**Rationale**: Configure routes, management port, full capture, session headers, diagnostics and Markdown projection with retained stage.

**Violation example**: Fail Gateway/stage creation or record an invalid loopback gateway_listen.

Execution and assertions: [Scrut S-DOC-041](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/gateway/S-DOC-041.md).

### S-DOC-042: H02 JSON recording


Suggested use: Connect Gateway, route models or record trajectories.

**Semantics**: Selected file is nonempty and first line resembles a JSON object. Each line should be an Event; this case checks only creation and first-line shape.

**Rationale**: Record Run Events as JSONL.

**Violation example**: Create an empty file or a non-object first line.

Execution and assertions: [Scrut S-DOC-042](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/gateway/S-DOC-042.md).

## I. Specs and control plane

Use these for automation/control planes or file-based RunSpec handoff. I01 uses TOML, I02 JSON delegation and I03 an extensionless file.

### S-DOC-043: I01 TOML config


Suggested use: Execute TOML/JSON configuration or RunSpec through a control plane.

**Semantics**: Command comes from the file without repeating it on CLI; Run completes normally.

**Rationale**: Create pvisor.toml with command = ["/bin/true"] under [run], then use --config; the script supplies it.

**Violation example**: Ignore file command or return wrong name/terminal state.

Execution and assertions: [Scrut S-DOC-043](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/config/S-DOC-043.md).

### S-DOC-044: I02 JSON RunSpec


Suggested use: Execute TOML/JSON configuration or RunSpec through a control plane.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Name is case-i02 and run-result.json is nonempty. Delegation currently supports host only and does not apply ordinary Session rootless safe profiles; this is not an isolation-mode example.

**Rationale**: Execute prepared JSON RunSpec and atomically write its result. Manually supply run_id, agent and process invocation; fixtures use /bin/true named case-i02.

**Violation example**: Omit the result or misrepresent delegation as an isolated ordinary Session.

Execution and assertions: [Scrut S-DOC-044](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/config/S-DOC-044.md).

### S-DOC-045: I03 Extensionless spec


Suggested use: Execute TOML/JSON configuration or RunSpec through a control plane.

**Semantics**: --config reads TOML and completes without a .toml suffix.

**Rationale**: Save I01 TOML as config-without-extension; the script supplies it.

**Violation example**: Reject the same TOML due to its filename.

Execution and assertions: [Scrut S-DOC-045](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/config/S-DOC-045.md).

## J. Combined configurations

These regress multiple capabilities: J01 host, J02 VM, J03 container. Short test commands cannot replace real agent workload acceptance. Diagnose failures by returning to A–I cases.

### S-DOC-046: J01 Host + persistent stage + deny-all + capture + limits


Suggested use: Validate combined capabilities before deployment.

Preparation: Linux user/mount namespaces or macOS Seatbelt available.

**Semantics**: Original workspace is unchanged; stage includes result.txt; result retains stdout/resource requests; selected JSONL records Events; networking is denied.

**Rationale**: Combine host stage, deny-all, output capture, JSON Events and limits while writing a result in the isolated view.

**Violation example**: Leak result.txt into the workspace or omit trajectory/resource/network evidence.

Execution and assertions: [Scrut S-DOC-046](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-046.md).

### S-DOC-047: J02 VM + image rootfs + stage + OverlayNet + Gateway


Suggested use: Validate combined capabilities before deployment.

Preparation: Linux with access to /dev/kvm. Set PVISOR_CASE_IMAGE for the script. PVISOR_CASE_AGENT must execute inside the guest.

**Semantics**: VM uses requested memory and retains stage/trajectory directory. Conversation output depends on actual Gateway calls; current assertions do not inspect conversation contents.

**Rationale**: Run a real agent in an image with its dependencies, retaining stage, smoltcp, Gateway and JSONL. Replace the placeholder upstream with a real service.

**Violation example**: Lose stage/trajectory or 4GiB memory request.

Execution and assertions: [Scrut S-DOC-047](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-047.md).

### S-DOC-048: J03 Container + stage + read-only root + no network


Suggested use: Validate combined capabilities before deployment.

Preparation: Working OCI runtime; set PVISOR_CASE_CONTAINER_IMAGE for the script.

**Semantics**: Container succeeds and leaves a stage. /bin/true generates no changes or meaningful stdout; this does not test stage writes or network blocking.

**Rationale**: Combine persistent stage, read-only rootfs, isolated network and stdout capture. Rootfs read-only and writable stage are different settings.

**Violation example**: Fail container completion/stage retention.

Execution and assertions: [Scrut S-DOC-048](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/container/S-DOC-048.md).

## K. Session review and lifecycle

Session is the user-facing object. These commands use its stage path directly as selector without another session subcommand.

### S-DOC-049: K01 Review and inspect staged files read-only


Preparation: Linux user/mount namespaces available.

**Semantics**: Review lists one file; the read-only view reads staged, rejects writes and leaves the original workspace unchanged.

**Rationale**: Use status --review then inspect staged content and verify inspect cannot write.

**Violation example**: Allow inspect writes or put note.txt in the original workspace.

Execution and assertions: [Scrut S-DOC-049](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-049.md).

### S-DOC-050: K02 Selective apply then discard remaining changes


Preparation: Linux user/mount namespaces available.

**Semantics**: Only one.txt reaches original workspace; two.txt never reaches lower.

**Rationale**: Apply one.txt, retain two.txt pending a decision, then explicitly drop remaining changes.

**Violation example**: Apply two.txt alongside one.txt or fail to discard the remaining stage.

Execution and assertions: [Scrut S-DOC-050](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-050.md).

### S-DOC-051: K03 Fork a stopped Session


Preparation: Linux user/mount namespaces available.

**Semantics**: Child reads inherited; neither Session writes changes directly to original workspace.

**Rationale**: Start a child from the source staged view, reading its changes and creating independent ones.

**Violation example**: Lose source changes or leak child writes into original workspace.

Execution and assertions: [Scrut S-DOC-051](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/checkpoint/S-DOC-051.md).

### S-DOC-052: K04 Terminate a running Session


Preparation: Linux user/mount namespaces available.

**Semantics**: Session exits before sleep completes; status --json reports cancelled and no live state.

**Rationale**: Background a long Session and request normal termination by stage path.

**Violation example**: Remain live after kill or report a different terminal state.

Execution and assertions: [Scrut S-DOC-052](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-052.md).

## L. Removed environment commands

The env feature was removed. S-DOC-053/S-DOC-054 are registered retired and never reused.

## M. Replay and interactive terminal

### S-DOC-055: M01 Prepare a replay prefix offline


**Semantics**: Output phase is prepared; historical commands did not create marker.

**Rationale**: Use a minimal native mini-swe-agent trajectory with replay --prepare-only. It parses the prefix without launching an agent or executing historical tools.

**Violation example**: Execute history/create marker or report replayed calls in prepare-only.

Execution and assertions: [Scrut S-DOC-055](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/replay/S-DOC-055.md).
