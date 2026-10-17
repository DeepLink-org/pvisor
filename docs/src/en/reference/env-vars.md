# Environment variables

Usually you only need to choose where Runs are stored and explicitly pass the credentials your agent needs. Host variables configure pVisor itself; environment projection determines what the task receives.

```bash
PVISOR_RUN_HOME="$HOME/.pvisor/runs" pvisor run --safe \
  --stage ../stage-env-001 --overlaynet-deny-all -- /bin/sh -c 'printf "%s\n" "$PVISOR_RUN_ID"'
```

The task prints its own Run ID. For network credentials, use `--pass-env` as described in [Credentials and environment](../guides/policies/credentials.md).

## Common host settings

| Variable | Current behavior |
| --- | --- |
| `PVISOR_RUN_HOME` | Default Run root; otherwise `~/.pvisor/runs`, or system temporary storage without HOME |
| `PVISOR_IMAGE_STORE` | OCI cache override; explicit `--image-store` wins |
| `PVISOR_CACHE_SERVER` | Shared cache endpoint; `off` disables; unset probes the per-user Unix socket |
| `PVISOR_CACHE_TOKEN` | Shared secret for TCP cache endpoints; do not pass it to agents with `--pass-env` |
| `PVISOR_BIN` | Obsolete Python-launcher override; installed native scripts do not read it. Invoke the binary path directly or select it through `PATH` |
| `XDG_CONFIG_HOME` | User-policy root, default `~/.config` |
| `HOME` / `PATH` | Host storage/tool-discovery/projection inputs; safe mode redirects HOME |

See [shared cache](shared-image-cache.md) for endpoint grammar, security, and failures. Host settings and agent-visible variables are separate; use `--pass-env NAME` for explicit projection.

## Daemon credentials {#daemon}

`OPEN_SANDBOX_API_KEY` supplies the standalone `pvisor-daemon serve` lifecycle key. Use a protected random secret of at least 32 bytes; do not pass it in argv or project it to workloads. Clients send it as the `OPEN-SANDBOX-API-KEY` HTTP header. The environment name uses underscores; the HTTP name uses hyphens.

Default daemon endpoints return a sandbox-scoped `X-PVISOR-SANDBOX-TOKEN` in endpoint headers; clients must preserve it. It is not a host environment setting or a lifecycle credential. See [endpoint authentication](../guides/daemon/operations.md#endpoints). Native cache/pool credentials and Job environment projection remain separate. Retired Cluster tokens are not daemon credentials.

## Runtime injection

`PVISOR_RUN_ID`, `PVISOR_RUNTIME`, `PVISOR_STORAGE`, `PVISOR_AGENT`, and `PVISOR_ROLE` identify runtime context. `PVISOR_AGENTCTL_ENDPOINT`, `PVISOR_AGENTCTL_TOKEN`, `PVISOR_AGENTCTL_TRANSPORT`, and `PVISOR_AGENTCTL_VERSION` support the optional cooperative Guest AgentCtl channel. The token is a Guest-only credential: it cannot authorize Host Job, VM or daemon-supervisor operations and should not be logged.

Proxy mode also injects upper/lowercase `HTTP_PROXY`, `HTTPS_PROXY`, and `ALL_PROXY`. These direct cooperating clients; they do not establish mandatory isolation alone. The Bundle `environment` field lists the actual projected names.

`PVISOR_KRUN_RUNNER_SPEC` and `PVISOR_KRUN_NETWORK_FD` are internal launch protocol. `PVISOR_KRUN_LOG` and `PVISOR_KRUN_ENOMEM_WORKAROUND` are VM diagnostics, not stable configuration. Use the tables above to configure the host; leave internal launch variables to the launcher and diagnostic tools.

## Startup diagnostics and source builds {#build-and-diagnostics}

`PVISOR_STARTUP_TIMING` enables startup phase logs by default; set it to `0` to disable them for measurements without logging overhead. It changes diagnostic output, not task policy.

`PVISOR_VM_FS_WRITEBACK` controls guest writeback. New staged VM sessions default to off; `1` explicitly requests it and `0` disables it. Restored sessions without an override preserve their negotiated mode; explicit `0` rejects a writeback-enabled snapshot. It is mutually exclusive with the host write cache below. Validation of this firmware/host fix covers Linux x86_64/KVM, without extending that evidence to macOS.

The 0048 firmware negotiates bit 42 and applies pressure using actual dirty pages and temporary FUSE WRITE pages, without accumulating user write bytes. Each connection/BDI has a soft pressure target of one eighth of actual kernel RAM, clamped to 16–256 MiB, and retains at most 256 handle leases. Submission and completion use separate queues. Clean pagecache is outside this target; independent rootfs/workspace BDIs can each have a target, while Linux global dirty/reclaim rules still constrain the total. The native BDI separately attempts a 16 MiB minimum reservation, warning on failure; it is not a fixed-byte guarantee.

The subsequent 0049 patch sets the early-submission hint to one quarter of the pressure target, clamped to 1–16 MiB. Small writes still progress on the 10 ms timer, and the negotiated maximum WRITE remains 1 MiB. Queue admission reads an approximate dirty-page counter outside the connection lock, avoids repeating the per-folio pressure check, and does not schedule an empty pending list. Actual pressure checks, both WRITE barriers around integrity writeback, and error propagation remain. Version 0049v2 also preallocates bounded scatterlist metadata before starting request virtqueues and reuses it under the existing VQ lock. File pages and per-request argument buffers retain their original completion ownership; other atomic allocations remain.

Historical 0048 evidence and the new 0049v2 cohorts remain separate. The new firmware passed Linux x86_64/KVM FULL/ERROR checks, seven firmware contracts, five replays of the original pressure history, independent N5 curves, and an ordinary-CLI WB0/1 comparison. Its correctness receipt retains the 50 Python contracts run at that point; subsequent CLI contracts have separate records. The CLI seven-tool total and Ready time show no detected benefit, and new sessions remain default-off. See the [independent experiments](../design/filesystem-io-performance-analysis.md#guest-writeback-tuning) for scope, workload results, and provenance.

Read `/sys/module/fuse/parameters/pvisor_wb_budget_bytes` inside the guest to inspect the active target. This diagnostic parameter is read-only through sysfs and rejects boot-argument assignment. Writable close may complete later; read-only close retains native FLUSH, and fsync/O_SYNC and final syncfs retain their error boundaries. The host retains upper object references or logical lower attributes after unlink/rename replacement until FORGET. Snapshots with detached/orphan references return Unsupported; finite legacy nodes with unknown snapshot counts remain conservative until DESTROY. Complete Agent IOCP/offload support remains unvalidated.

`PVISOR_VM_FS_HOST_WRITEBACK` enables an experimental host write cache, disabled by default: unset or `0` disables it, `1` enables it, and other values are rejected. It currently requires Linux staged virtio-fs, guest writeback disabled and DAX disabled. Each connection allows 64 MiB of payload and 1024 outstanding items; adjacent contiguous writes batch up to 1 MiB or 10 ms. Explicit synchronization and exit still observe background errors. Complete Agent IOCP and automatic offload remain pending. See [EXP-003](../design/experimental/agent-io-completion/index.md#host-write-buffer) for scope and synchronization contracts.

`PVISOR_VM_FS_HOST_WRITEBACK_CONFIG` is a private engineering JSON control: `capacity_mib` is 4–256, `jobs` 1–4096, `batch_kib` 4–4096 and no larger than capacity, `chunks` 1–64, and `delay_us` 0–50000. Unset preserves the defaults above; unknown or duplicate fields, incorrect types and out-of-range values are rejected. `qualification_lock` and `synchronous` provide the old qualification lock and an owned-worker completion ablation. These are not stable product settings. See the [filesystem I/O experiments](../design/filesystem-io-performance-analysis.md) for curves and scope.

The following configure the guest kernel embedded **at build time** in Linux x86_64 musl builds:

| Variable | Input | Precedence |
| --- | --- | --- |
| `PVISOR_KRUNFW_KERNEL_BUNDLE` | Directory containing `kernel.bin` and `kernel.json` | Takes precedence when set |
| `PVISOR_KRUNFW_PATH` | `libkrunfw.so.5` file from which to extract the kernel | Used when the bundle is unset |

Rebuild the CLI after changing these inputs. Setting them while running an already-built musl binary does not replace its embedded kernel. See [Platform support](platforms.md) for dynamic firmware entry points and platform requirements. The build implementation is `crates/pvisor-vm/build_kernel.rs`.
