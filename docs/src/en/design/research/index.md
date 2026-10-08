# Research directions

OS and MLSys research concerns correct task completions under a fixed resource budget and the human cost of handling their results. pVisor supplies execution, state and evidence mechanisms; orchestrators and trainers compose workloads and evaluate benefits through controlled experiments.

## From mechanisms to falsifiable conclusions {#research-loop}

![Research problems, hypotheses, mechanisms, controls, lifecycle metrics and conclusions](../assets/research-cycle.svg)

A useful hypothesis specifies workloads and counterexamples. VMs with equal baselines and read-heavy access may benefit from shared pages; private writes reduce sharing, while scanning and COW peaks can erase ready-stage savings. Experiments pin correct outputs and budgets, and conclusions retain their scope.

## Execution and orchestration ownership {#ownership}

pVisor owns individual execution boundaries and observations; the node-local daemon owns sandbox identity and lifecycle. External orchestration owns host selection, queues, dependencies and application retries; trainers own models, sampling, reward and evaluator versions. The [daemon API](../daemon/index.md) does not expose native Job staging, execution checkpoints or Run Bundle export, so research integration needs an explicit execution/evidence handoff.

A proposed adapter binds external work identities to local Jobs/Attempts or sandboxes, pins inputs and runtime revisions, and collects results with artifacts. Timeouts and disconnects preserve unknown outcomes for reconciliation before retry. [External orchestration research](cluster-execution.md) examines this handoff; placement remains external.

## Three kinds of state and evaluation {#state}

| State | Retained content | Separate evaluation |
| --- | --- | --- |
| File checkpoint | Candidate files, conflict baselines and lineage | Branch isolation, publication correctness, copy-up and storage costs |
| Agent trajectory | Model context and tool history | Prefix preparation, re-execution differences and model continuation |
| VM execution checkpoint | CPU/RAM/device/file state of supported profiles | Consistency, compatibility, restoration and fork costs |

Tool re-execution produces fresh observations; remote service state and training metadata are retained separately. Execution success, task reward and file acceptance are evaluated independently; see the [RL substrate](rl-execution-substrate.md) for the proposed integration.

## Levels of benefit evidence {#evidence}

![Evidence from execution requests to accepted artifacts](../assets/evidence-flow.svg)

- **Mechanism evidence:** verify bytes, mappings, write isolation, references and failure states.
- **Workload evidence:** pin tasks, inputs, output checks, model capacity and total resources; record correct completions, first results, restoration tails, CPU, peaks and memory-time.
- **Product evidence:** include preparation, SDKs, guest services, reclamation, sustained concurrency and failure scope.
- **Supervision evidence:** measure actual review effort, rejection and rework; machine CPU cost does not measure human attention.

Native pool or offload probes alone do not establish complete daemon density. Controller/Worker data retains its historical identity. Orchestration and RL adapters remain research/planning work, with benefits requiring their own workload validation.

## Using designs and papers {#references}

[Public research outputs](publications.md) retain materials, revisions and experiment scope; no formal papers, external reports or talks are registered yet. [Open-source design references](design-documents.md) explain problem statements, layer diagrams, sequences, failure semantics and tradeoffs without turning writing practices into product guarantees.
