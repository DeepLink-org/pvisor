# pVisor application frontends

`pvisor-cli` owns three executables: `pvisor`, `pvisor-tui`, and
`pvisor-replay`, with their argument parsing, terminal adapters, rendering and
companion lookup.
The TUI frontend lives here, not in the embeddable runtime.

## Boundary

Embed [`pvisor`](../pvisor/README.md) for `PVisor`, Session/Attempt execution and
`job_service::RuntimeJobService`. This application library exposes frontend
modules, not a compatibility re-export of the runtime. Frontends call explicit
runtime APIs and retain request-local cancellation, admission fences and terminal
ownership. Each running Job owns its recorded private control socket; stopped Job operations
run directly in the caller. Embedded execution publishes the same discovery record.

The separate `pvisor-daemon` owns shared services, including the cache server and memory pool.
Use `pvisor cache` to build, publish and query caches; invoke `pvisor-daemon serve --cache`
to serve them. `pvisor daemon start/status/stop` dispatch to the same-installation
daemon companion. Replay and TUI are also root companions. Use `pvisor -- COMMAND`
for explicit workload execution.

Companion lookup resolves the running executable's real installation directory,
so Homebrew-style entry symlinks work on macOS as well as Linux. Companions in
that directory must remain executable regular files owned by the current user or
root, with no group/world write permission; the installation directory has the
same ownership and write-permission requirements. Lookup does not search `PATH`.

Recording callers import `Journal` and `JournalStore` from `pvisor_journal::api`;
durable stage preparation uses its `Persistence` and `DurableFiles` contracts.

## Command help and live VM controls

Root help starts with **Commands** (container `run`, `build`, `pull`, `images`, `rmi`,
`exec`, `ps`, `start`, `stop`, `rm`, `logs`), **Agent Commands** (Job `spawn`, `stage`,
`status`, `kill`, `review`, `apply`, `drop`, `inspect`, `checkpoint`, `suspend`,
`resume`, `fork`) and **Tools** (installed `daemon`/`replay`/`tui` companions, `cache`, `feature`,
`help`). It includes the run-review-apply examples; use `pvisor help COMMAND`
for command options.

```bash
pvisor spawn --safe -- claude
pvisor review last
pvisor apply last --path src
```

`apply SOURCE` defaults to the recorded project. SOURCE is required. With no
selection, it opens a picker only when both stdin and stdout are terminals;
explicit `--path`, `--include` or `--all` executes without prompting unless
`--interactive` is requested. `--all --exclude GLOB` is allowed; `--all` cannot
combine with `--path` or `--include`. Exclusions alone require `--interactive`.
See the [apply reference](../../docs/src/en/reference/cli.md#apply) for the complete
argument, JSON and exit-code contracts.

TOML `[run] profile = "safe"` selects the same preset as `--safe`. Explicit
`--filesystem host` conflicts with the safe profile and fails before workload
execution. Ordinary runs report `safe_profile_requested = false`. Managed replay
uses `spawn --config`, preserves the safe request and returns the outer exit status.
After an interrupted new Attempt, `status` reconciles a committed completion or
reports `unknown` while preserving its stage; missing completion is never success.
`status --review` and `review` display `UNKNOWN` for an uncommitted completion,
with the retained stage and an existing `status` reconciliation entry point.
Their `--json` form returns `state: "unknown"`, `COMPLETION_UNCOMMITTED`, reason,
identities and recovery selector instead of a fabricated Bundle or safety summary.
This read-only diagnostic exits successfully; it does not authorize apply/drop.
`status <selector> --json` reconciles available receipts under the existing locks;
a missing receipt keeps the outcome unknown. Corrupt evidence remains an error.

The seven live VM flags are command-local, under **Live VM** in command help:

| Command | Accepted live VM flags |
| --- | --- |
| `status` | `--vm-socket`, `--vm-job-id`, `--vm-attempt-id` |
| `suspend` | The three identity flags plus `--vm-pause`, `--vm-offload`, `--vm-ram-file` |
| `resume` | The three identity flags plus `--vm-load` |

Place these flags after the supported command. Normal status/suspend/resume
resolve the owner from the Job record. `--vm-socket` is a debug override under
`/tmp/pvisor-host-<UID>` and requires the two explicit identity flags; positional
selectors must then match `--vm-job-id`. Pause and offload are mutually exclusive;
`--vm-ram-file` requires offload. Resume with a live owner continues the same
Attempt; stopped resume restores a sealed checkpoint.

The frontend self-executes one owner over an inherited private socketpair. The
owner remains in the original session, owns descendant cleanup, and installs
signal handlers before unblocking signals. Interactive Jobs use the original
controlling terminal and may receive Ctrl-C directly; foreground ownership is
restored at exit. Other commands never start a worker. Live commands execute
inside the starting owner binary through `pvisor-instance-control-1`; compatible
caller binaries need not have matching digests. Stopped operations take the Job
root's `job.lock`. Apply/drop refuse an owner still holding stage. See the runtime
README for socket validation, lost-response ambiguity and platform limitations.
The explicit `--vm-socket` debug override still uses the older SDK exchange;
its response wait currently ignores CLI cancellation. Runtime Overlay and native
capture sockets also remain in use. See [internal channels and cleanup order](../../docs/src/en/design/architecture.md#owner-internal-control).

Help, version and feature queries emit no startup logs. Execution startup markers
are disabled by default; set `PVISOR_STARTUP_TIMING=1` to enable checkpoints,
persistence timings and frontend/owner correlation records. This changes diagnostics
only; workload output, boundary/staging notices, warnings, errors and Run Bundle/review
guidance remain visible. Enabled markers are emitted after command parsing and do
not measure complete process entry.
Execution lets clap build the selected parsing path; grouped root help still
builds the full tree. The generated `help` command never defaults to `spawn`.
`PVISOR_STARTUP_PROFILE=1` additionally records `cli.entry_to_parsed`, measured
from this frontend's `main` entry and emitted only after informational commands
have returned. This includes internal dispatch checks and parsing, but excludes
the OS loader before `main`. Checkpoints `cli.owner_image_begin/ready`,
`cli.owner_spawn_begin/returned`, and `cli.owner_request_sent` distinguish image
verification, process creation and request dispatch. Owner bootstrap remains a
bound aggregate, including its own verification before environment restoration.
The [startup diagnostic collector](../../benchmark/startup/startup_profile.py)
keeps process-relative spans separate from host checkpoint gaps and workload Ready.

The frontend verifies and pins its own executable before launching its owner;
there is no user-wide digest admission or generation socket. For upgrade, drain
old Jobs and stop the old `--pvisor-internal-host` with the old binary first.
New CLIs read old records without live control as disk state only.

## Staged system experiments (Linux preview)

`stage` creates an ordinary Job in a native VM with the host `/` as a live,
read-only lower. Unlike `spawn --rootfs host`, it retains the **whole rootfs**
upper and preimages after exit, not just workspace edits. No image is needed.
The lower is not a frozen system snapshot; concurrent host changes remain visible.

```bash
pvisor stage -it                         # shell; omission of COMMAND selects $SHELL or /bin/sh
pvisor stage --network none --stage ./experiment /bin/sh -c 'echo changed > example.txt'
pvisor review ./experiment --diff
pvisor apply ./experiment                # choose project changes in a terminal
pvisor drop ./experiment
```

The interface follows the container CLI's option spelling and argument boundary:
`-i/--interactive`, `-t/--tty`, repeated `-e/--env` and `--env-file`,
`-w/--workdir`, `--name`, `--cpus`, `-m/--memory`, and `--network bridge|none`.
There is no positional image. Options after COMMAND (or `--`) belong literally
to the workload. `--name` is the Job label, not a new name-based selector;
use the printed Job ID, stage path, or `last` in the launching workspace.

Defaults are 2 vCPUs, 512 MiB RAM, current host working directory, and controlled
IPv4 TCP/DNS egress (`bridge` uses the Job's existing VM data plane, not membership
in the Docker container bridge). `none` omits guest networking. A command's stdin
is closed unless `-i` is supplied; no COMMAND selects a shell with stdin attached
and uses a PTY when launched from a terminal. `-t` requires a host terminal and
allocates a guest PTY, combining stdout/stderr; without `-i`, stdin remains
`/dev/null` even with `-t`. Without `-t`, explicit commands use separate non-TTY streams. Stage uses the ordinary Job owner, cancellation,
completion receipt, review, and drop; it does not appear in container `ps`.
There is no detach, `--rm`, writable bind mount, image selection or host fallback.
Personal Agent defaults are not loaded. Existing workspace/network policy defaults
still apply. `PVISOR_STAGE=1` identifies the environment to scripts.

`--stage PATH` selects fresh persistent storage; omitted storage is
`PVISOR_RUN_HOME/JOB_ID` or `$HOME/.pvisor/runs/JOB_ID`. Existing Job storage is
never silently reused. Exit, including a nonzero workload exit, retains reviewable
changes. `drop` discards only pending changes, not already applied paths or external
side effects. HOME and other readable host files, including credentials, remain
visible: this is write staging, not a confidentiality boundary. Cache management
credentials and implicit loader paths retain the VM's existing environment scrub;
explicit `-e` values have Docker precedence. Unset entries remain absent, including
HOME, PATH and TMPDIR.

### Applying project changes

For new `stage` Jobs, ordinary `apply` selects only changes beneath the recorded
launch-time project, even though the stage retains the whole rootfs. Paths and
globs are project-relative; outside-project changes remain staged. Non-root
project apply is supported within this restricted contract:

```bash
pvisor apply ./experiment
pvisor apply ./experiment --path src --dry-run --diff
pvisor apply ./experiment --all --exclude 'build/**'
pvisor apply ./experiment --path src --dry-run --json
```

The picker starts with nothing checked when no positive selection was supplied.
Use ↑/↓ to move, Space to toggle, `a` for all eligible paths, `n` for none,
`d` for a diff, Enter for preview, and `q` to cancel. Preview offers `a` to apply,
`b` to go back, `d` for a diff, and `q` to quit. Explicit selection with
`--interactive` preselects matching paths. Execution checks the preview's
expected-plan token; a changed plan requires a fresh preview and confirmation.

`--dry-run` does not write the target, journals, ledger or recovery state and
does not consume staged changes. Preview locks open only existing lock files
and leases: missing `job.lock` or required lease files fail closed, without
creating them. New `stage` startup precreates these observation locks. Apply
still requires a stopped Job and valid completion evidence.

New rootstage Jobs capture `ProjectApplyScope` version 1: a canonical project
root other than `/`, with its device/inode identity. The scope is bound into the
RunRecord and Run Bundle by the completion receipt and revalidated before
writeback. A changed directory identity or missing/mismatched binding blocks
apply. Older rootstages without that launch contract cannot gain project
authority from the current directory or `--target`; recreate the stage for
project apply. Ordinary workspace stages keep their existing interface and
legacy explicit `--target` compatibility, limited to an existing workspace
directory; this does not grant rootstage project or system-root authority.
New rootstage project apply does not accept a target override.

The restricted rootstage project path supports regular file additions, edits
and deletions, plus required new directories. It rejects symlinks, special
files, opaque changes, directory deletion/replacement, existing-directory
metadata changes and project-root changes. Non-root callers also need
owner-readable logical file modes: a logical `0200` write-only file is rejected
before writes because crash recovery could not safely reread it. New directories
must be caller-owned and owner-readable/searchable. Private backing permissions
do not relax these logical-mode checks.

No matching changes is a successful no-op: execution prints
`No matching staged changes`. A pending apply instead returns `blocked` without
implicit recovery. Keep the stage, apply ledger and recovery evidence for
inspection and explicit reconciliation; retrying is not an automatic recovery
procedure, and no new recovery CLI is provided. An execution error can leave
partial target writes. Apply does not run tests automatically or provide a
multi-file transaction; validate the result separately and stop concurrent
writers. Exit codes are `0` success/no changes, `2` argument errors, `3` blocked,
`1` execution/service errors and `130` cancellation.

### Applying system changes

Advanced `apply --rootfs` explicitly selects the recorded system `/` target instead of
the project. It requires a positive selection (`--path`, `--include` or `--all`)
or terminal interaction. Paths are relative to `/`:

```bash
# Initial privileged workflow: create and review as root, in root-controlled storage.
sudo -H pvisor stage -it --stage /root/pvisor-stages/config-edit
sudo -H pvisor review /root/pvisor-stages/config-edit --diff
sudo -H pvisor apply /root/pvisor-stages/config-edit --rootfs --path etc/example.conf
sudo -H pvisor drop /root/pvisor-stages/config-edit
```

This preview **does not elevate or import an ordinary user's stage**. Same-UID
Job locks are preserved; rootfs apply requires effective UID 0 and a root-created
stage whose canonical directory and ancestors are root-owned and not group/world
writable. `/tmp` storage is therefore not eligible. `--rootfs` conflicts with
`--target`, and ordinary apply cannot redirect a workspace stage to `/`.

Rootfs apply supports ordinary file changes and required new ancestor directories.
Directory deletion/replacement, opaque replacements and root metadata changes
are rejected before preparing a new apply. Select supported file paths or drop
unsupported changes. Private backing preserves logical guest ownership/modes;
its physical owner-access permissions and private control xattrs are not published
to the host. Importing source objects with POSIX ACLs is currently unsupported and
fails closed rather than changing ACL semantics. Other special-file and
unprivileged ownership limitations remain those of the VM/OverlayCore adapter.

**This is not an atomic system upgrade or rollback mechanism.** Package files,
configuration and package databases can be interdependent; arbitrary path selection
may leave them inconsistent. Native VM success does not validate host services,
devices, the bootloader, initramfs or the next boot. Review and back up the system
before any privileged apply; stop concurrent target writers. Network API calls,
database writes and sent messages cannot be undone by drop. Linux x86_64/KVM is
the initial supported target; macOS host-root staging is rejected explicitly.

Validation: `mise run test -- pvisor-cli -- -E 'binary(stage_cli)'` checks ordinary
CLI contracts. With KVM and matching GNU firmware available, set
`PVISOR_TEST_LIBRARY_DIR=/absolute/firmware/directory` and run
`mise run test -- pvisor-cli -- -E 'binary(stage_cli)' --run-ignored only` for the
native host-root workflow. It writes only isolated temporary fixtures and never
applies to the real host root.

## Docker command profile (experimental)

`pvisor run` creates a container; `spawn` creates a Job with stage/review/apply
semantics. Unknown command names and leading Job options default to `spawn`;
`pvisor -- COMMAND` explicitly selects default Job execution. The former Job
`run` syntax must now use `spawn`. Each container
uses its own Linux VM and supervisor, started as the current host user. No
central Docker daemon, privileged helper or shared Linux VM is required.
Guest root is independent of host root; the image's `User` is respected.
Docker defaults are translated on the host into generic guest launch options.
Jobs, container workloads and exec share one guest launcher and PID-1 reaper;
the optional control transport does not implement a separate supervisor.

```bash
pvisor pull alpine:3.20
pvisor run --rm alpine:3.20 echo hello
pvisor run -d --name redis -p 127.0.0.1:6379:6379 redis:7
pvisor exec -it redis redis-cli
pvisor logs -f redis
pvisor stop redis
pvisor start redis
pvisor rm -f redis
pvisor images
pvisor rmi redis:7
```

Opt in to the shared daemon memory pool on Apple Silicon macOS or Linux:

```bash
# Optional: start explicitly; auto also starts it when absent.
pvisor daemon start --memory-pool
pvisor run -d -M --name redis redis:7
pvisor spawn -M --rootfs image=alpine:3.20 -- /bin/echo hello
pvisor daemon status
pvisor daemon stop # Stops only the manager; retains the pool and live VMs.

# Explicit instances/sockets remain supported:
pvisor-daemon serve --memory-pool --state /tmp/pvisor-pool
# In another terminal, using the socket printed by the daemon:
pvisor run -d --memory-pool /tmp/pvisor-pool/memory-pool/pool.sock --name redis redis:7
```

`-M` is shorthand for `run --memory-pool auto` or `spawn --vm-memory-pool auto`.
It takes no argument, so `pvisor run -M alpine` keeps `alpine` as the image.
It conflicts with an explicit pool option. The long forms remain supported.
`auto` discovers a same-user daemon independent of the current directory, or
starts a detached manager with a memory pool. It requires the installed
`pvisor-daemon` beside `pvisor`; PATH cannot supply a replacement. Omission of the
pool option starts no daemon. An explicit socket is never auto-started/replaced.
The default private instance directory is canonical `/tmp/pvisor-daemon-UID`;
export `PVISOR_DAEMON_HOME=/absolute/private/directory` in all client shells to
select a custom instance (`pvisor daemon ... --state DIR` overrides it for that
command). Start/status JSON advertises the protocol, manager identity and pool
configuration. Capabilities are fixed at manager startup; absent capabilities,
incompatible protocols/modes, failed readiness and lost pool owners are errors,
with no silent fallback. Logs are in the instance's `manager.log`.
`--vm-memory-pool auto` also works through frontend TOML `[vm] memory_pool = "auto"`;
the runtime receives a resolved socket. Embedding callers still pass explicit paths.
Job owners ask the launching frontend to resolve `auto` over their private
bootstrap channel. Shared services therefore survive Job completion and
request descendant cleanup; ordinary workload descendants remain owned and cleaned.

The private same-user socket is canonicalized and saved with the container;
`start` reuses it and fails if unavailable. Guest mounts/rootfs must not expose
the pool directory or an ancestor. Apple Silicon uses a private, unlinked RAM
backing file. The default daemon mode compresses cold blocks and restores private
RAM on access. Opt-in `--memory-pool-mode physical-pages` on the daemon instead
merges equal resident 16 KiB pages: reads stay shared and writes use COW. No macFUSE RAM mount is required. **On macOS,
pool loss terminates dependent VMs.** Stopping the service manager leaves its
detached pool alive; drain containers before stopping that pool. Linux negotiates
the existing physical or sealed-compressed protocol. Omitting `--memory-pool`
preserves ordinary container RAM behavior. The feature is experimental; encoded
pool size does not establish net host-memory savings. See the [daemon contract](../pvisor-daemon/README.md#optional-daemon-owned-memory-pool).

On Apple Silicon, `--shared-kernel-base DIR` additionally enables a common,
read-only boot-kernel baseline with private COW writes. The daemon creates a
private `<state>/memory-pool/shared-kernel` directory and includes its canonical
path in standalone readiness JSON. For example:

```bash
pvisor run -d --shared-kernel-base /tmp/pvisor-pool/memory-pool/shared-kernel alpine sleep 3600
```

Containers using the same directory and firmware reuse one backing inode.
This flag shares initial kernel RAM. Add a `physical-pages` daemon pool to
merge equal runtime heap/cache pages as well; host 16 KiB page alignment limits
which guest pages can merge.
The flag works independently of `--memory-pool`, or alongside it: both pager modes
exclude the shared kernel. A baseline-only container retains its mappings after
daemon exit or cache-path removal. Snapshot profiles and whole-VM offload are
unsupported for this mode. Keep the directory private and outside guest mounts;
never modify/truncate its files. Cache files have no automatic GC and are outside
the compressed-pool byte budget. The feature is opt-in and does not establish a
net memory-saving percentage. `stop`/`start` retains the directory selector.

Pulls report metadata resolution, per-layer downloaded bytes and transfer rate,
checksum verification, extraction and cache hits. Progress goes to stderr: a
terminal redraws the active layer's bar; redirected output uses plain, throttled
status lines. Extraction has a phase indicator rather than an estimated percent.
The same display is used when `run` automatically pulls a missing image.

`pull --platform linux/amd64` or `linux/arm64` selects the image architecture;
`linux/arm64/v8` is accepted as an alias. Omission selects the native platform.
Pulling a foreign image is supported, but `run` and `start` reject foreign images
before launching a VM: CPU emulation is not implemented. `run --platform` asserts
the native platform, including for cached images. Each local reference holds one
platform; pulling the same tag for another platform replaces that reference,
while existing containers keep their pinned images.

The registered commands are `run`, `build`, `pull`, `images`, `rmi`, `exec`,
`ps`, `start`, `stop`, `rm`, `logs` and `network`. Compatibility is limited to the options
listed in `pvisor COMMAND --help`; unsupported commands/options fail
rather than being silently ignored. This is not complete Docker CLI or
Docker Engine API compatibility. There is no Compose, Docker socket,
Docker credential-helper integration or cross-architecture emulation.

`stop -t/--timeout` accepts nonnegative seconds (default 10); `-1` waits
indefinitely after delivering the requested stop signal, without force-stop
escalation. Values below `-1` are rejected before contacting a container.

| Command | Implemented options |
| --- | --- |
| `run` | `--platform`, `--name`, `--rm`, `-d`, `-i`, `-t`, `-e`, `--env-file`, `-w`, `-u`, `--entrypoint`, `-v`, `-p`, `--cpus`, `-m/--memory`, `-M`, `--memory-pool`, `--shared-kernel-base`, `--network`/`--net bridge/none/NAME`, `--ip` |
| `build` | Local context, `-f`, repeated `-t`/`--build-arg`, `--target`, `--platform`, `--pull`, `--no-cache`, `-q`, `--iidfile`, `-o type=oci,dest=PATH`, `--network default/none`, `--cpus`, `-m`, `--timeout`, `--progress auto/plain` |
| `pull` | `--platform`, one public OCI image reference |
| `images` | Optional repository, `-q`, `--no-trunc` |
| `rmi` | Multiple references/IDs, `-f` |
| `exec` | `-i`, `-t`, `-e`, `-w`, `-u`, command and arguments |
| `ps` | `-a`, `-q`, `--no-trunc` |
| `start` | Multiple container names/IDs |
| `stop` | Multiple names/IDs, `-t/--timeout`, `-s/--signal` |
| `rm` | Multiple names/IDs, `-f`, `-v` |
| `logs` | One container, `-f` |
| `network` | `create --driver bridge --subnet --gateway`, `ls -q --no-trunc`, `inspect`, `rm -f` |

The first profile supports Apple Silicon macOS/HVF and Linux x86_64/KVM;
Linux runtime acceptance must be validated separately. macOS binaries require
the existing Hypervisor entitlement and firmware. Each `--cpus` unit is a
whole vCPU, not a fractional Docker CPU quota. `--memory` sizes total guest
RAM. Public IPv4 TCP/DNS egress uses existing OverlayNet policy; general UDP,
IPv6 and host networking are not implemented. Cross-container networking is
available on the default shared `bridge` and user-defined networks below.
Default bridge has no container-name DNS; user-defined networks do. `-p` publishes TCP through the owning VM's guest control
transport; omitted host addresses bind `0.0.0.0`, matching Docker syntax.
An exposed image port does not publish itself.

Directory bind mounts require an existing source. Named and anonymous volumes
are managed separately from instance uppers; empty managed volumes are
initialized from the image destination. Special-file initialization is
unsupported. An empty managed volume must first be initialized through a
writable mount before it can be mounted read-only. Read-only shares reject
mutation requests in the host adapter as well as in the guest mount. `stop`/`start` retains the instance upper and volumes; `rm` drops
the upper, `rm -v` also drops anonymous volumes, and named volumes survive.
There is not yet a volume-management CLI or image-blob garbage collector;
`rmi` removes local references while retaining shared pool objects and legacy OCI data.

TTY allocation defaults `TERM` to `xterm` when no image/container or explicit
value is present; explicit values, empty values and unsets retain precedence.
Exec terminal defaults do not change the container environment.
TTY allocation supports initial terminal dimensions; live resizing and detach
key sequences are not implemented. stdout/stderr retain separate log streams
without a merged timestamp ordering or `--since`/`--tail` filtering. Current
human-readable list output is a pVisor rendering; scripts should use the
implemented quiet options. Read the supported profile rather than assuming
that every flag of a registered Docker command is available.

Storage defaults to the current user's local data directory under
`pvisor/docker`. `PVISOR_DOCKER_HOME` selects an isolated store, including for
tests. It must be private to the current user and cannot be a bind-mount source
or ancestor. Images share the [local image store](../../docs/src/en/reference/shared-image-cache.md#local-image-store)
with spawn and the cache server; `PVISOR_IMAGE_STORE` selects it. Without that
override, an explicit Docker home keeps images in its own `images` directory.
New pulls and builds use that image store's `pool-v2`: the cache's
compressed directory radix index and shared seekable-Zstd content pool. Small
files pack by directory; large files use independent 1 MiB chunks. VM reads go through the direct lazy
backend; ordinary reads do not accumulate a second raw disk cache. Full OCI
archives and extracted rootfs trees are temporary import/build inputs, except
explicit `--output` exports. Original configurations remain under the image store's `configs`.
Old directory-based records continue to work and are not automatically removed.
Pool revisions remain pinned in container records across tag replacement/removal.
Short and fully qualified Docker Hub references address the same catalog tag.
Replacing or removing it through any alias affects that one identity; existing
containers and distinct tags retain their pinned revisions.
Filesystem/S3 publishers use `--image-store` as the parent of scoped
`staging/.import-*` directories, removed after success or error. No raw OCI
directory is retained. `SSL_CERT_FILE` can supply a host registry CA bundle.
Socket clients and the VM bridge require protocol V2; unsupported peers fail
without automatic downgrade, even when connection reuse is disabled.

V1 storage handles and pool encodings are retired. Re-pull to publish an image to
the new pool, then recreate containers pinned to old handles; old data is never
automatically deleted. Filesystem, S3 and pool use the same V2 physical format.
Arbitrary xattrs are rejected because the current pool format cannot preserve
them. Cold random reads may decode up to a 1 MiB chunk; mmap uses ordinary guest
page caching, with no new DAX path or measured performance claim. See the
[runtime storage contract](../pvisor/README.md#independent-container-vm-lifecycle).
Per-generation control sockets live below an owned `0700`
`/tmp/pvisor-docker-UID` directory, keeping macOS Unix socket paths short.

## Prepared VM rootfs

`pvisor cache prepare-rootfs --rootfs DIR --base-store STORE` is a local operation,
independent of cache daemons, backends and registry access. The source must be
quiescent; preparation copies/verifies it and commits a persistent store pin
before printing `{schema_version, prepared_rootfs: {store, base: {id}}, rootfs}`.
Cache backend/location/read-only/image-store options conflict with this command.
The source and store must not contain one another. The private store must remain
outside guest-visible or writable workspace/stage/mount trees and must not be
modified manually. No prepared-entry removal command is provided.
Both local source and destination are checked against canonical Host authority
before initializing/copying, without contacting or changing a listener.

`pvisor spawn --vm --rootfs-base-store STORE --rootfs-base ID -- COMMAND` opens that
catalog entry in the ordinary Job owner, before default host rootfs or
OCI preparation. Both options are required together and conflict with `--rootfs`,
configured image sources, host/container execution and JSON `--spec`. TOML can
use `[vm.prepared_rootfs]` with `store` and `base = { id = "..." }`; old configs
retain their defaults. Jobs persist the canonical selector and exact root path;
future owners must authenticate both rather than trust the path alone.

Warm opens verify the small record/seal and marker, then retain a GC lease in the
executor and native child; they do not reimport or fully hash the tree. Durable
catalog pins also preserve stopped Jobs after the preparer exits. Rootfs ownership
does not snapshot the workspace, promote OCI/lazy paths, or enable the default-off
lower-data/local-FLUSH experiments. Native execution checkpoint capture and
restore are unsupported for this source: the capability and Job admission report
that limit before capture, and no capture binding is installed. Workspace logical
checkpoints retain their independent behavior. See the runtime README for the
ownership and platform limits.

Successful owner process-tree cleanup is idempotent. Linux uses subreaper
adoption and pidfds, observes stopped forkers before rescanning, and reaps escaped
descendants inside the owner. The frontend reaps its own owner child. After
Attempt terminal publication, the owner allows the existing bounded RAM/macFUSE
cleanup window before escalating. macOS keeps
birth-identity checks, polling and its existing limitations for missed reparented
orphans. These paths require platform-specific integration coverage.

## Prepared VM workspace

An explicit prepared workspace reads one fixed, owned generation while retaining
the live caller workspace as the apply destination. Prepare it outside the Run:

```bash
pvisor cache prepare-workspace --workspace /opt/workspace-source --base-store /opt/workspace-bases
pvisor spawn --vm --rootfs-base-store /opt/rootfs-bases --rootfs-base ROOTFS_ID \
  --workspace-base-store /opt/workspace-bases --workspace-base WORKSPACE_ID -- /bin/sh
```

The preparation command is local, with the same backend-option conflicts and
canonical source/store/Host-authority checks as `prepare-rootfs`. It prints
`{schema_version, prepared_workspace: {store, base: {id}}, workspace}` after the
workspace-purpose catalog pin commits. A rootfs catalog record cannot substitute
for a workspace record. Preparation copies a quiescent source once; warm Runs
authenticate the catalog and seal without reimporting the source or auditing the
whole tree. The private store and generation must remain externally unmodified.

Run options require the workspace pair together. TOML uses
`[overlayfs.prepared_workspace]` with `store` and `base = { id = "..." }`.
`PreparedBase` is the common selector DTO; the existing `PreparedRootfs` Rust name
and `vm.prepared_rootfs` JSON/TOML representation remain compatible. A conflicting
CLI/config workspace selector fails. Jobs persist its canonical store/id for
independent later owners; serialized fields do not carry leases or ownership.

The first supported path requires a native VM, an authenticated prepared rootfs,
independent rootfs/workspace stores, one lower and fresh stage backing. It rejects
JSON `--spec`, host/container execution, staged compose layers, additional shares,
custom guest workspace positions, workspace checkpoint fork, and native execution
restore. The default live
workspace path is preserved inside the guest. Stores must be disjoint from that
live target, stage, root share, mutable VM backing, recording output and any
guest-visible sources, including canonical symlink aliases. Existing stage or
Run metadata is not permission to reopen or overwrite a previous Job.

The Job owner reopens the workspace-purpose catalog and passes its
typed lease with the exact prepared-rootfs `Arc<VmExecutor>` to the runtime. Fresh
upper/compact-journal creation occurs under the Run lease. The native child
independently authenticates the physical seal and holds its own lease; it does
not recheck catalog purpose. Completion/cancellation notifications retain their
pre-teardown timing through a read-only runtime executor-output observer.

Reads use the selected generation even when live files change. Staged mutations
retain that generation as their baseline; manual apply targets the live workspace
and keeps normal content-conflict detection. Immutable lower does not mean the
live apply target is protected. No new VM cache, writeback or DAX setting is
enabled by this selector. Default Runs retain their existing live-workspace
semantics. These interface changes do not establish a performance result.

## Source layout

- `src/cli/`: Job commands, typed owner bootstrap, per-Job discovery,
  safe Agent presets, terminal adapters and output rendering.
- `src/cli/features.rs`: runtime feature listing frontend; registry/settings
  remain in `pvisor::features`.
- `src/cli/cache.rs`: cache command parsing and rendering; cache storage,
  image preparation and authenticated server implementation remain in `pvisor`.
- `src/companions.rs`: trusted same-installation companion discovery/dispatch.
- `src/tui/`: native TUI PTY runtime, renderer, review panels and keymap,
  including Zellij attribution and the adapted border-glyph license.
- `src/bin/pvisor-replay.rs`: replay argument parsing and managed-run frontend;
  the independent replay engine remains in `pvisor-replay`.
- `src/bin/`: executable entry points.
- `tests/`: executable/frontend integration tests, including tests that combine
  runtime APIs with command execution. Runtime-only tests remain in `pvisor`.

The runtime README retains detailed execution, control, storage, service and
platform limits. Its command examples refer to these installed frontends.

## Build and validation

`gateway` forwards to `pvisor/gateway`; default builds keep Gateway optional.
Use `mise run test -- pvisor-cli` for application tests and `mise run test -- pvisor` for
runtime tests. Native VM and ignored environment-dependent gates retain their
existing prerequisites; compile checks are not real-guest validation.

Linux OCI fixtures distinguish an inner deadline (an untrusted child process
exit) from the host transport watchdog. The latter must terminate and reap an
unresponsive runtime; delegated result files use the writable `output/` mount.
Fixtures resolve input beside the OCI bundle; rootfs mountpoint placeholders
are not serialized input and must not be discovered by a recursive file search.

The ignored `minimal_guest_without_dev_repeatedly_reaches_ready_with_captured_stdio`
Job test needs KVM, a static C compiler and (for GNU builds)
`PVISOR_TEST_LIBRARY_DIR` pointing to local firmware. It boots an image containing
only a static Ready executable ten times each with one and two vCPUs, checks
unique Ready output and zero exit, and keeps every attempt's logs on failure.
It covers PID 1's pre-Rust standard-stream repair when the image has no `/dev`.

The Docker native acceptance test uses an isolated store and a native Linux
rootfs containing `sh`, `cat` and `sleep`. On macOS it signs a private binary
copy with the existing Hypervisor entitlement. Run it with local socket and
virtualization access:

```bash
PVISOR_DOCKER_TEST_ROOTFS=/absolute/path/to/rootfs mise run test -- pvisor-cli -- \
  -E 'binary(docker_cli)' --run-ignored only
```

It checks startup failure, exit codes, stdout/stderr, stdin EOF, PTY allocation,
detached lifetime, stop/start persistence, foreground signal forwarding, orphan
reaping and concurrent exec exit routing. A separate ordinary Job VM case checks
the default launch configuration's orphan reaping and exit status.

Set `PVISOR_DOCKER_TEST_IMAGE` to a pinned native Alpine reference to run the
container/build acceptance cases on freshly pulled compressed-pool images;
the fixture asserts a pool binding instead of installing a directory-backed
image. `PVISOR_DOCKER_TEST_ROOTFS` remains required by the separate ordinary Job
case. `PVISOR_DOCKER_TEST_BINARY` can select the static product executable with
embedded firmware, independently of Cargo's dynamic test artifact. It is copied
privately before use; macOS copies retain the existing signing procedure.


## VM-isolated image builds (experimental)

```bash
pvisor build -t app:local --network=none .
pvisor run --rm app:local
pvisor build -f Dockerfile -t app:local --build-arg VERSION=1 \
  --output type=oci,dest=app.oci.tar .
```

`build` reuses the image catalog, OverlayCore and the container VM launch path.
Every `RUN` executes in a fresh native VM as the host's ordinary user, with only
its private rootfs overlay writable. The build context is captured without
following symlinks; it is copied by `COPY`, never mounted into a RUN VM. No host
shell, Docker socket, host bind mount or privileged build mode is exposed.
The frontend owns each VMM through a private pipe; timeout/error reaps it, and
frontend death closes the pipe and terminates it. A stage's filesystem persists
between RUNs; guest processes do not. `--network=none` omits the NIC. The default
network retains the existing public IPv4 TCP/DNS restrictions.

The Dockerfile subset includes `FROM`/`AS`, completed-stage `COPY --from`,
`RUN` (shell/JSON), local `COPY` (JSON, globs, numeric `--chown`, octal `--chmod`),
`ARG`, `ENV`, `WORKDIR`, `USER`, `CMD`, `ENTRYPOINT`, `SHELL`, `LABEL`, `EXPOSE`,
`VOLUME` metadata and `STOPSIGNAL`. `.dockerignore`, negation and Dockerfile-specific
ignore files are supported. ARG values are not persisted as image environment;
explicit `--build-arg NAME` forwards only that host variable. Build arguments
are not secret storage. Simple `$NAME`/`${NAME}` expansion is supported.

Each finished stage becomes one flattened OCI layer with guest ownership/mode,
symlinks, hardlinks and merged deletions preserved. No instruction cache is
implemented (`--no-cache` is accepted); unchanged base images can be reused.
Tags are published after the shared content and immutable index are committed. A failed
step leaves existing tags unchanged. OCI output must name a new file and contains
only the standard layout/config/manifest/layer blobs, not the private rootfs.
Builds and instances share `PVISOR_DOCKER_HOME`; temporary builds do not appear in
`ps` and are removed on ordinary failure/success. Abrupt frontend death can leave
an unpublished temporary build directory.

This is a bounded Docker build profile, not full BuildKit compatibility. Remote
or stdin contexts, `ADD`, `ONBUILD`, heredocs, RUN mounts, secrets/SSH/cache mounts,
external-image `COPY --from`, named COPY owners, extended variable substitutions,
foreign architectures, device nodes and file capabilities are rejected. Source
paths traversing a symlink are rejected; copied symlinks remain symlinks. There
are no registry push, Docker archive output, instruction cache or multi-platform
builds. Native acceptance is exercised on Apple Silicon/HVF; Linux/KVM requires
separate acceptance. Rootless Linux preserves default guest-root symlink owners
in OCI output without host chown; non-root symlink owners are rejected when the
host cannot represent them. Guest-visible symlink-owner virtualization is not
claimed by this build profile.

## Same-host peer networks

The container network CLI follows Docker's network names and IPv4 options:

```bash
pvisor network create --subnet 172.20.0.0/24 dev
pvisor run -d --name web --network dev --ip 172.20.0.11 \
  python:3.12-alpine python -m http.server 8000
pvisor run --rm --network dev alpine:3.20 wget -qO- http://web:8000/
pvisor network ls
pvisor network inspect dev
pvisor stop web
pvisor rm web
pvisor network rm dev
```

`run --network` (alias `--net`) selects `bridge` by default, `none` for no NIC,
or an existing network name/unique ID prefix. The former `--peer-network` and
`--peer-address` options are rejected. The default bridge is a shared
`172.17.0.0/24` network; it supports IP-address communication without container
name DNS. User-defined networks provide live container-name DNS. A service bound
only to guest loopback is not reachable from another container. Port publication
is needed only for host access, not for member-to-member traffic.

`network create` accepts `-d/--driver bridge`, `--subnet` and `--gateway`.
Only canonical IPv4 /24 subnets in RFC1918 or `192.0.2.0/24` are supported.
Omitting the subnet allocates a free /24 from `172.18.0.0`–`172.31.255.0`.
Gateway defaults to the first host. Existing networks in this store cannot overlap.
`run --ip` requires a user-defined network and a non-gateway host address in its
subnet. Without it, an address is allocated under the catalog lock. Addresses
remain reserved across stop/start and are released by container removal.
Network names follow container-name syntax (1–128 ASCII letters/digits and
`_`, `.`, `-`, starting with a letter/digit). Container DNS labels are at most
63 bytes and must be unique on a network ignoring case. Network definitions and reservations are scoped to
`PVISOR_DOCKER_HOME`, persisted in that store and survive CLI restarts.

`network ls` supports `-q/--quiet`, `--no-trunc` and alias `list`.
`network inspect NAME...` emits Docker-style JSON including IPAM and attached
container addresses. This is a supported CLI subset, not Docker Engine API
compatibility. `network rm NAME...` (alias `remove`) refuses built-ins and any
container attachments, including stopped containers. Its `-f/--force` only
ignores missing networks and never disconnects containers.

Each logical bridge uses direct Unix sockets with independent peer send budgets;
no host bridge or packet broker is created. The catalog lock is used only for
network configuration and address reservation. IPv4 TCP, UDP, ICMP and targeted
ARP are supported between members. External traffic retains ordinary TCP/DNS
egress. MTU is 1500. IPv6, non-/24 subnets, host/container modes, multiple network
attachments, `network connect/disconnect`, DNS aliases, general broadcast/multicast
and IPv4 fragments are unsupported and rejected/dropped explicitly.

Native Job `spawn --config` and embedded callers retain explicit
`[overlaynet.peers]`/`PeerNetworkSettings` for low-level user-private groups,
independently of the container store catalog. See the
[runtime contract](../pvisor/README.md#explicit-same-host-vm-peer-networks).

The Job owner calls `VmExecutor::prepare_process()` after self-exec and environment
restoration, before creating Tokio threads. Linux can then grow its high-FD table
while it is unshared. The temporary descriptors are closed immediately; no existing
FD, reserved runner destination or resource limit changes. Preparation failure is
nonfatal so a low descriptor limit does not disable host Jobs. A failed later VM
FD duplication retains its normal error. Other platforms keep their existing path.

## Optional ephemeral Job stages

For disposable jobs that do not need apply or checkpoint/restart recovery:

```bash
pvisor spawn --stage /path/to/fresh-stage --stage-durability ephemeral -- COMMAND
```

The equivalent configuration is `[overlayfs] durability = "ephemeral"`.
The default remains `checkpoint`; `strict` remains available. Ephemeral completion
retains ordinary output and validates observations, but omits the automatic
upper/preimage tree sync. It publishes a volatile marker instead of a durable
seal. Apply (including auto-apply), logical/execution checkpoints, and reuse
for another Attempt are unsupported. Read-only inspection does not certify
post-crash output. Use a new stage for each job.

Guest fsync, normal writes, preimage hashing, audit/Job metadata persistence and
RAM offload writeback are unchanged. This is a stage completion option, not a
global persistence-off switch. Any comparison must disclose the reduced stage
guarantee and use the same binary/input for the two durability settings; no
performance or cross-runtime fairness claim follows from enabling it.

To also omit execution-time observations used for apply/recovery:

```bash
pvisor spawn --stage /path/to/fresh-stage --stage-durability ephemeral \
  --stage-recording off -- COMMAND
```

`--stage-recording on|off` defaults to `on`; `off` requires `ephemeral`, including
when settings come from TOML. The equivalent config is `[overlayfs]` with
`durability = "ephemeral"` and `recording = "off"`. This skips per-file journals
and their fingerprints, not ordinary writes, explicit fsync, image integrity
checks, execution results or audit logs. Outputs remain inspectable, but cannot
be applied, checkpointed or reused. Existing ephemeral behavior is unchanged
without this additional option. No performance benefit has yet been measured
for recording-off mode.
