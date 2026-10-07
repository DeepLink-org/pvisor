# Core design principles

Operation is the object being processed, pvisor owns the execution lifecycle, and Event provides the observation interface.

## Separate definitions from execution

Shared operations, policies, Placement, outcomes and interaction contracts belong in core. Admission, scheduling, process launch, sockets, resource preparation and lifecycle belong in pvisor and drivers. Find an existing production caller before adding a type; do not add another executor or control protocol for a future interface.

## Separate requests, decisions and facts

Requests describe intent, policy decisions describe effective constraints, Placement describes the selected execution location, and observations describe what happened. Rewrites preserve both original and effective snapshots rather than overwriting the request. See [Capabilities and evidence](../concepts/capabilities-and-evidence.md) for the boundary between plans and actual controls.

## Preserve known causal dependencies

If a step depends on an earlier result, it must wait for that result. Placement, forwarding and recording cannot reverse the dependency. Causal references express known dependencies, Journal positions express commit order, and timestamps express observation time. None replaces another or establishes a global order of effects across Jobs.

## One lifecycle owner

Session owns resource preparation, execution, cancellation, cleanup and terminal publication for an Attempt. Drivers report output and observations rather than publishing the terminal state of the entire Job. Cancellation is a request; completion is a result. Recording failures and execution failures are reported separately.

## Guarantees require actual boundaries

Actual interception mechanisms provide file and network controls. Guarantees must state platform and coverage. See [Capabilities and evidence](../concepts/capabilities-and-evidence.md) for `null` counters, capture coverage and sources of enforcement.

## Review files before applying them

With staging enabled, workspace changes stay in the Overlay; explicit review and apply change the target. Failed execution can still leave reviewable changes. See [Capabilities and evidence](../concepts/capabilities-and-evidence.md) for recovery scopes and irreversible external effects.

## Records reconstruct facts

Public records should explain requests, actual rewrites, placement, results and provenance. Snapshots and causal chains make facts reviewable. Deterministic replay of effects also requires initial state, external inputs and the corresponding execution mechanism; Event records alone are insufficient.

See [Core architecture](architecture.md), [Operation and Event](operations-events.md) and [Isolation design](isolation.md) for mechanisms.
