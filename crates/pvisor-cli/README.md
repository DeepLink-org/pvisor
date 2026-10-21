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
to serve them. Replay and TUI are root
companions. Use `pvisor -- COMMAND` for explicit workload execution.

Recording callers import `Journal` and `JournalStore` from `pvisor_journal::api`;
durable stage preparation uses its `Persistence` and `DurableFiles` contracts.

## Command help and live VM controls

Root help starts with **Commands** (container `run`, `pull`, `images`, `rmi`,
`exec`, `ps`, `start`, `stop`, `rm`, `logs`, plus Job `spawn`, `status`, `kill`), **Changes**
(`review`, `apply`, `drop`, `inspect`), **Checkpoints** (`checkpoint`, `suspend`,
`resume`, `fork`) and **Tools** (installed `replay`/`tui` companions, `cache`, `feature`,
`help`). It includes the run-review-apply examples; use `pvisor help COMMAND`
for command options.

```bash
pvisor spawn --safe -- claude
pvisor review last
pvisor apply last --path src
```

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
are emitted after command parsing and do not measure complete process entry.
The frontend verifies and pins its own executable before launching its owner;
there is no user-wide digest admission or generation socket. For upgrade, drain
old Jobs and stop the old `--pvisor-internal-host` with the old binary first.
New CLIs read old records without live control as disk state only.

## Docker command profile (experimental)

`pvisor run` creates a container; `spawn` creates a Job with stage/review/apply
semantics. Unknown command names and leading Job options default to `spawn`;
`pvisor -- COMMAND` explicitly selects default Job execution. The former Job
`run` syntax must now use `spawn`. The old `pvisor docker COMMAND` spelling
remains a hidden compatibility entry point with the same container dispatcher. Each container
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

The registered commands are exactly `run`, `pull`, `images`, `rmi`, `exec`,
`ps`, `start`, `stop`, `rm` and `logs`. Compatibility is limited to the options
listed in `pvisor COMMAND --help`; unsupported commands/options fail
rather than being silently ignored. This is not complete Docker CLI or
Docker Engine API compatibility. There is no `build`, Compose, Docker socket,
Docker credential-helper integration or cross-architecture emulation.

| Command | Implemented options |
| --- | --- |
| `run` | `--platform`, `--name`, `--rm`, `-d`, `-i`, `-t`, `-e`, `--env-file`, `-w`, `-u`, `--entrypoint`, `-v`, `-p`, `--cpus`, `-m/--memory`, `--network bridge/none` |
| `pull` | `--platform`, one public OCI image reference |
| `images` | Optional repository, `-q`, `--no-trunc` |
| `rmi` | Multiple references/IDs, `-f` |
| `exec` | `-i`, `-t`, `-e`, `-w`, `-u`, command and arguments |
| `ps` | `-a`, `-q`, `--no-trunc` |
| `start` | Multiple container names/IDs |
| `stop` | Multiple names/IDs, `-t/--timeout`, `-s/--signal` |
| `rm` | Multiple names/IDs, `-f`, `-v` |
| `logs` | One container, `-f` |

The first profile supports Apple Silicon macOS/HVF and Linux x86_64/KVM;
Linux runtime acceptance must be validated separately. macOS binaries require
the existing Hypervisor entitlement and firmware. Each `--cpus` unit is a
whole vCPU, not a fractional Docker CPU quota. `--memory` sizes total guest
RAM. Public IPv4 TCP/DNS egress uses existing OverlayNet policy; general UDP,
IPv6, host networking, private-address egress and cross-container discovery
are not implemented. `-p` publishes TCP through the owning VM's guest control
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
`rmi` removes local references while retaining the internal OCI cache.

TTY allocation supports initial terminal dimensions; live resizing and detach
key sequences are not implemented. stdout/stderr retain separate log streams
without a merged timestamp ordering or `--since`/`--tail` filtering. Current
human-readable list output is a pVisor rendering; scripts should use the
implemented quiet options. Read the supported profile rather than assuming
that every flag of a registered Docker command is available.

Storage defaults to the current user's local data directory under
`pvisor/docker`. `PVISOR_DOCKER_HOME` selects an isolated store, including for
tests. It must be private to the current user and cannot be a bind-mount source
or ancestor. Per-generation control sockets live below an owned `0700`
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
Use `just test pvisor-cli` for application tests and `just test pvisor` for
runtime tests. Native VM and ignored environment-dependent gates retain their
existing prerequisites; compile checks are not real-guest validation.

The Docker native acceptance test uses an isolated store and a native Linux
rootfs containing `sh`, `cat` and `sleep`. On macOS it signs a private binary
copy with the existing Hypervisor entitlement. Run it with local socket and
virtualization access:

```bash
PVISOR_DOCKER_TEST_ROOTFS=/absolute/path/to/rootfs just test pvisor-cli -- \
  -E 'binary(docker_cli)' --run-ignored only
```

It checks startup failure, exit codes, stdout/stderr, stdin EOF, PTY allocation,
detached lifetime, stop/start persistence, foreground signal forwarding, orphan
reaping and concurrent exec exit routing. A separate ordinary Job VM case checks
the default launch configuration's orphan reaping and exit status.
