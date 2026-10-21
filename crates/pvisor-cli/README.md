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

Root help stays concise: **Execution** (`run`, `status`, `kill`), **Changes**
(`review`, `apply`, `drop`, `inspect`), **Checkpoints** (`checkpoint`, `suspend`,
`resume`, `fork`) and **Tools** (installed `replay`/`tui` companions, `cache`, `feature`,
`help`). It includes the run-review-apply examples; use `pvisor help COMMAND`
for command options.

```bash
pvisor run --safe -- claude
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

`pvisor run --vm --rootfs-base-store STORE --rootfs-base ID -- COMMAND` opens that
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
pvisor run --vm --rootfs-base-store /opt/rootfs-bases --rootfs-base ROOTFS_ID \
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
