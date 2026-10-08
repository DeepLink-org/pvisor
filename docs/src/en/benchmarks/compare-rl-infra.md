# Where can pVisor reduce reinforcement-learning rollout costs?

## Main conclusions {#conclusions}

**For local tool-heavy attempts, stage offers lower measured repair cost and higher tested active capacity than pVisor VM; choose VM when an independent guest kernel is required. Prefix preparation and lazy image reads address other rollout costs, with no demonstrated improvement in training throughput, success rate or reward.**

| Scenario | Selection implication |
|---|---|
| Many short repository attempts | Evaluate stage for retained edits and active capacity; its boundary is a host process |
| Independent guest kernel per attempt | Budget VM tool cost and lower tested capacity, even with short startup waiting |
| Historical context or uncached images | Evaluate prefix preparation or lazy reads within their measured scope |
| Existing RL stack | Keep task, model and training layers; dedicated integrations remain unvalidated |

## Motivation {#motivation}

A rollout creates an environment, alternates model inference with tool calls, runs verification, assigns a reward and retains or rejects the sample. Failed attempts may need context preparation, environment restoration and another execution. Choosing an executor requires budgeting these stages separately and preserving the task's required boundary.

## Experiment design {#interpretation}

This scenario analysis reuses registered local Linux/x86_64 experiments; it makes no new measurements. The owning topics define artifacts, validation and controls:

- [Active capacity](density.md): shared 2 CPU / 2 GiB / zero swap, five fresh batches per cell, barrier-released Python/Git tasks with 32 MiB private data; failures remain in capacity denominators.
- [Repair](agent-tasks.md): prepared offline tools, warm caches, three warmups, 60 randomized fixed-plan trials per backend; completion includes launch and exit, excludes inference and preparation. CPU affinity is controlled, but host/container memory is uncapped and tool VMs have 16 GiB.
- [Replay](replay-fidelity.md): twenty synthetic short prefixes per adapter, three shuffled repetitions, no warmups or cache clearing; exact historical arguments/observations, zero executed tools and unchanged workspace are required.
- [Startup](startup.md) and [lazy images](lazy-image-startup.md): separate prepared-shell and cold/warm-client experiments; lazy services use local loopback and warm host caches. [Isolation](isolation-tests.md) checks final host/stage state; [VM memory](vm-memory/index.md) uses separate N=1 static readings.

Invalid outputs and failed executions do not enter successful timings; all valid slow samples remain. Counts and separated timing clusters are retained. These cohorts have different memory, cache and workload controls: do not pool them, sum their medians or turn them into a full RL cost ranking.

| Component | Complementary responsibility and limit |
|---|---|
| [OpenHands runtime](https://docs.openhands.dev/openhands/usage/sandboxes/docker) | Agent tool environment, including Docker sandbox; a replay-format check does not validate a runtime integration |
| [SWE-Gym](https://github.com/SWE-Gym/SWE-Gym) | Repository tasks, executable environments, verification and Agent/verifier training; no matched training comparison |
| [verl](https://verl.readthedocs.io/en/latest/) | Training, rollout and model resource coordination; no validated pVisor training integration |
| pVisor | Per-attempt execution, staged files and records; model inference, GPU allocation, reward design and cluster scheduling remain external |

## Data and analysis {#results}

The following observations support environment decisions, not effective training samples per second.

| Rollout cost / check | Current evidence and statistic | Scope for decisions |
|---|---|---|
| Active tool capacity, 2026-10-06 | Stage 32: 5/5 batches, 160/160 tasks; Podman 32: 5/5, 160/160; VM 16: 5/5, 80/80; VM 32: 3/5, 138/160 | Shared 2 CPU / 2 GiB; idle counts do not establish active capacity |
| Fixed repair through exit, 2026-10-06 | Completion P50: staged 0.64 s, Docker 0.81 s, VM 3.25 s, QEMU microvm 1.40 s; each 60/60 valid | Prepared tool plan; differing kernels, filesystems and caches prevent a pure VMM ranking |
| Prepared shell startup | Ready P50: staged 25.13 ms, VM 100.91 ms; N=60 each | First output excludes later tool work; separate from repair and lazy cohorts |
| Prefix preparation, 2026-10-06 | Seven formats each 60/60; six have lower-cluster medians 6.07–6.59 ms and higher-cluster medians 11.15–16.24 ms; Mini-SWE-Agent unsplit P50 6.75 ms | No tool reexecution, Agent launch or workspace writes |
| Lazy cold-client shell, 2026-10-07 | Ready P50 183.7 ms; content 2,580,417 bytes (2.58 MB), versus Docker full OCI content 41,842,292 bytes (41.84 MB); N=30 each | Prepared local services; different delivery formats, no real WAN or training workload |
| Workspace boundary, 2026-10-06 | Staged/safe/VM retain writes and block tested outside accesses, 3/3 each; tested OCI/Podman writable mounts write through | Path/socket fixtures, not a comprehensive escape audit or remote-effect rollback |

The [repair comparison](agent-tasks.md#reference-env) reports staged minus Docker completion median −175.84 ms (95% CI −178.85 to −174.55 ms), and VM minus microvm +1849.00 ms (1834.43 to 1855.78 ms). Stage's advantage comes with a host-process boundary; VM supplies a guest kernel but costs more for this tool plan and fits fewer wholly successful active batches. Neither observation establishes model quality or general rollout speed.

Real-client compatibility is a separate cohort: [Codex in VM](agent-tasks.md#cli-compatibility) completes the controlled local-response loop at P50 9.01 s, N=30. Claude Code VM hits the 90 s initialization preflight deadline, with N=0 valid samples; there is no latency estimate for it. Fixed-plan results cannot establish universal Agent compatibility.

Lazy cold-client Ready excludes initial image-service preparation; with a cached image, Docker is usually faster. Content bytes exclude total wire traffic and cannot predict a large repository task's transfers. [VM memory choices](vm-memory/index.md) can help budget model-wait periods, but offload requires recovery and compression depends on contents; N=1 static readings and snapshot sizes do not establish parked or active rollout capacity.

Native Agent resume owns the client's session/context. Prepare-only supplies a checked historical prefix and leaves actual files unchanged; restore the task environment separately. VM checkpoint/fork owns captured execution state and associated resources, subject to lifecycle and backing prerequisites. No measured native-resume speed ranking, complete remote-connection restoration or rollback of remote API effects is available. Tool reexecution needs its own side-effect and correctness checks.

Training throughput, useful-sample success, reward, retries and end-to-end resource cost remain unmeasured. OpenHands, SWE-Gym and verl integrations need separate acceptance; an adapter's prefix fidelity is insufficient evidence.

### Downloads and reproduction {#run}

[Scenario evidence CSV](compare-rl-infra.csv) retains topic, source, cohort, statistic, counts, values and limitations. Source summaries: [density](density-summary.csv), [repair/CLI](agent-tasks.csv), [startup](startup.csv), [replay](replay-fidelity.csv), [lazy startup/payload](lazy-startup-summary.csv), [isolation](isolation-tests.csv), [VM memory](vm-memory/memory-choices.csv). Artifact digests and full controls stay with each owning topic's provenance downloads; raw evidence stays in local `.data/`.

[Comparison method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
