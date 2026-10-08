# Open-source design references

[ByteHook’s project introduction and principles](https://github.com/bytedance/bhook/blob/main/doc/overview.zh-CN.md) is the main writing reference for pVisor’s design chapter. It establishes prerequisite low-level models, diagrams objects and calls, then explains engineering choices and constraints. Readers reach implementation through causal reasoning.

For pVisor, the sequence is execution problems and ownership → VMs, vCPUs and virtio → memory mappings, sharing and faults → filesystems, images and network requests → snapshot consistency and journal commits → costs and experiments. Figures should show actual buffers, files, queues, references and state changes; prose follows each figure to explain why its steps are necessary.

[Design and underlying mechanisms](../index.md) connects these layers through a task that reads a file, accesses HTTPS and writes configuration. The projects below supply narrower references that complement this continuous explanation.

## Projects and specific articles {#projects}

These are recommendations about writing and organization, rather than project-quality or performance rankings. Sources were visited on 2026-10-07; implementation details retain each project's stated revision scope.

| Project and article | Useful writing practice | Application to pVisor |
| --- | --- | --- |
| [ByteHook · Introduction and principles](https://github.com/bytedance/bhook/blob/main/doc/overview.zh-CN.md) | Derives mechanisms from prerequisite models, diagrams objects and calls, and explains constraints | Connect syscalls, virtqueues, backing, preimages and commit points to the complete execution path |
| [TigerBeetle · Architecture](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/ARCHITECTURE.md) | Starts with workloads and constraints, then explains persistence and decisions through an overall model | Describe reuse, writes, failure and budgets before slots/references, Journal and snapshots |
| [Firecracker · Design](https://github.com/firecracker-microvm/firecracker/blob/main/docs/design.md) | Connects host integration, processes/threads and devices, showing fast paths and host responsibilities | Show API, supervisors, VMs, devices and pools; explain control and data paths separately |
| [gVisor · Architecture guide](https://gvisor.dev/docs/architecture_guide/intro/) | Uses syscall examples to explain Sentry/host relationships, mechanisms and compatibility | Trace one file/network request through actual interception and Host/OCI/VM coverage |
| [Tokio · Runtime](https://docs.rs/tokio/latest/tokio/runtime/index.html) | Moves from usage to thread lifetimes, fairness assumptions and scheduler behavior, separating guarantees from implementation | Explain waiter cancellation, accepted operations, completion tasks and runtime shutdown |
| [etcd · Client design](https://etcd.io/docs/v3.6/learning/design-client/) | Explains connections and requests through client responsibilities, errors and retries | Describe lost ACKs, Unknown, request IDs, retries and reconciliation; locate ambiguity in sequences |
| [Ray · Architecture whitepapers](https://docs.ray.io/en/latest/ray-contribute/whitepaper.html) | Separates overall architecture from internal topics and retains whitepaper versions | Separate orchestration, node ownership, execution paths and research; retain proposal status |
| [Rust Compiler Development Guide · Overview](https://rustc-dev-guide.rust-lang.org/overview.html) | Moves from processing flow to queries, data structures and source entry points, identifying simplifications | Trace Job/Attempt calls into core data, lifecycle and implementation files |
| [CockroachDB · Design](https://github.com/cockroachdb/cockroach/blob/master/docs/design.md) | Layer diagrams connect data abstractions, components and physical layout, with historical limits | Explain logical Jobs, persistent records and backing objects; identify historical designs |

Choose narrower references by the question: Firecracker for host integration, Tokio for concurrency guarantees, etcd for failed requests and retries. Keep pVisor’s own data and execution paths as the narrative, so readers can understand behavior without first memorizing a component catalog.

## Explaining one mechanism completely {#structure}

![From problems and hypotheses to mechanisms, experiments and scoped conclusions](../assets/research-cycle.svg)

1. **Problem and constraints.** Who encounters which cost? Which resources and trust assumptions are fixed?
2. **Minimal system model.** Who owns state, may mutate it and must release it?
3. **Normal flow.** Trace one real request through calls, data, commit points and results.
4. **Failure flow.** Explain retained state and reconciliation after cancellation, disconnects, write failure and process exit.
5. **Tradeoffs.** Describe costs removed and introduced, plus where alternatives fit.
6. **Implementation and evidence.** Provide source entry points and materials; distinguish behavior, workload results and proposals.

For Journal, explain event identities and idempotent retries before diagramming append, write, sync and receipts. Write failure retains Unknown; retrying the same event does not create a second record. Readers can understand failure semantics without interpreting LocalSync as remote replication. [Journal design](../journal.md) owns the detailed contract.

## Choosing figures and their detail {#figures}

| Reader's question | Suitable figure | Essential information |
| --- | --- | --- |
| Who owns each responsibility? | Layer or ownership diagram | Owners, boundaries, call direction and external dependencies |
| How does an operation complete? | Sequence or lifecycle diagram | Acceptance, commit, confirmation, cancellation and return |
| Where is data stored? | Physical layout or mapping diagram | Objects, references, offsets, private/shared state and reclaim conditions |
| What survives a crash? | Failure-state diagram | Durable intent, live observations, Unknown and reconciliation |
| Why is it cheaper or slower? | Sourced experiment figure | Budget, baseline, samples, failures, peaks and applicable workload |

Each figure answers one main question. Arrows identify calls, data, references or state changes; each figure explicitly defines its solid and dashed lines. SVG retains searchable text and scalable geometry; prose supplies an example and the main tradeoff. Component relationships should map to code or contracts.

The overall architecture matches the documentation with a navy background, blue-gray layers, light-blue headings and nested rounded shapes. Execution mechanisms remain the visual focus, with distinct outlines expressing nesting. Within `pvisor-vm`, vCPUs, RAM and virtual devices appear side by side, above their coordinated freeze and platform adaptation. Shared contracts and optional extensions remain supporting annotations. VM, memory, filesystem and network block diagrams reuse this visual language to expand their respective boundaries. Each topic explains the diagram and ownership, one access path, core data and mechanisms, failure and cleanup, then system connections and source entry points. Detailed mechanism figures use a textbook layered style: blue bands of different brightness separate execution layers, address spaces or phases; dark cards represent concrete objects; corresponding objects stay aligned across layers or before/after states. Numbers connect to the prose, and labels also express color meanings. Sequence diagrams retain their time axis, byte layouts retain offsets and proportions, and long figures split into independently readable details.

Figures use `#0d1b30` for the canvas, `#142840` for cards, `#a8cefa` for accents and `#e9eef8` for text, matching the site theme. Teal, amber and muted red retain each figure's distinctions between states such as sharing, changes and conflicts, alongside text labels. SVGs contain their complete palette so standalone viewing and PNG exports retain the same appearance.

## Separate designs, decisions and research {#status}

Implementation designs describe current ownership and behavior; [ADRs](../decisions/index.md) retain context, options and consequences; research retains hypotheses, experiments and counterexamples. External articles guide expression. pVisor's implementation status and performance claims remain constrained by its own revisions and evidence.
