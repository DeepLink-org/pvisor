# The PolicyVisor model

**Policy-governed, reviewable execution.**

PolicyVisor, abbreviated **pVisor**, supervises the execution of Agent CLIs, scripts, and automation commands. The **p** stands for **Policy**: the permissions and constraints for a Run, the runtime mechanisms that implement them, and the evidence available for review.

Your existing command remains the workload. PolicyVisor manages the execution around it, independently of its reasoning framework or business logic.

## From policy to a reviewable result

1. **Declare authority.** Choose the executor, workspace, and capability requirements for the task. Enable `--stage` when file changes need review before reaching the project.
2. **Check capabilities.** Admission evaluates requested capabilities against the selected provider. Required controls must be supported; optional degradation must be visible in the Run record.
3. **Execute with controls.** The selected host, container, or VM executor and its runtime drivers provide the actual boundary. Network policy governs the traffic those mechanisms can mediate.
4. **Inspect the evidence.** Review the outcome, installed controls, warnings, and observed Effects in the Run Bundle. Apply selected staged files or discard the remainder.

Here, policy means the Run's capability requirements and runtime configuration. It does not imply a new policy language, a central policy service, or identical enforcement on every platform.

## What pVisor implements today

pVisor combines Run identity, host/container/VM executors, capability admission, OverlayFS staging, optional Gateway capture, network policy, and local execution records. Its user-facing loop is [run, review, and apply](../start/first-run.md). Agent-specific integrations, including model capture and cooperative AgentCtl, extend this loop when needed.

Requested authority, installed enforcement, and observed effects are separate facts. An explicit proxy mediates configured client traffic; it does not by itself block direct sockets. A filesystem checkpoint captures staged files, not process memory. Applying or dropping a stage cannot undo a remote API call.

## Design directions

Fleet placement, verifiable node attestation, cross-service effect transactions, and hostile multi-tenant isolation are design directions, not guarantees of the current local CLI. See the [roadmap](../development/roadmap.md) and [local-to-fleet discussion](../design/local-to-fleet.md).

Use the [capability evidence](capabilities-and-evidence.md) of a concrete Run to evaluate its boundary.
