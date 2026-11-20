# Release notes

## pVisor 0.4.0 {#v040}

Use `pvisor run` to run a container in its own VM or `pvisor spawn` to run a
task Session and review staged changes. Linux x86_64 and Apple Silicon macOS
have stable wheels and native archives. The experimental Windows x86_64 WHP
container profile is available from source. [Platform requirements](../reference/platforms.md) · [Install pVisor](../start/installation.md)

| Workflow | What you can do |
| --- | --- |
| Containers and Compose | On Unix, manage container-profile Sessions with Docker-style commands. Use experimental, daemonless `pvisor compose` for project startup, dependency checks and cleanup. [Compose](../reference/cli.md#compose) |
| Image distribution | Authenticate OCI pulls to a registry's Bearer token endpoint. Publish lazy images with `pvisor push` and import a pinned source with `pvisor image pull --from`. [Image backends](../reference/shared-image-cache.md) |
| Agent tasks | Monitor tasks from DSH and attach a terminal to eligible running Linux Host Sessions. The adapters prefer VM in `auto` mode and report the selected executor. [Agent plugins](../guides/agents/plugins.md) |
| VM reuse and reclamation | Fork eligible Linux/KVM containers on the same host with copy-on-write RAM. Enable offload/resume for fork children or enroll ordinary container VMs in the Linux pressure service with caller-managed wait boundaries. [VM lifecycle](../guides/infrastructure/lifecycle.md#container-memory) |
| containerd | Set node defaults for VM egress and optionally relay eligible guest TCP listeners within the pod network namespace. [Shim integration](../guides/infrastructure/integration.md#containerd) |
| File results | Apply returns failure and recovery evidence. Workspace snapshots exclude leased sibling stages. [Review and apply](../guides/review-apply.md) |

Replay collects resource samples separately from trajectory scheduling.
The release gate checks the acceptance receipt against the wheel and source
commit. See the [release process](releasing.md) and [feature tests](testing.md).
