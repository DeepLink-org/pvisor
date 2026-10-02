# When you do not need pVisor

If **all three** conditions hold, an agent sandbox or Docker may be enough:

- You only need a disposable environment whose file changes you can discard with `git checkout`.
- You need neither selective path merging nor refusal to overwrite your concurrent edits.
- You need neither checkable execution evidence nor semantics shared across agents/executors.

## Other poor fits

| Situation | Reason | Suggested approach |
| --- | --- | --- |
| Main risk is production APIs, database writes, or messages | Staging cannot undo external effects | Service-side permissions/approval or a test environment |
| Mutually untrusted tenants | pVisor is not a hostile multi-tenant boundary; macOS VMM retains caller host permissions | A dedicated multi-tenant sandbox/virtualization platform |
| Cryptographic execution proof | Local records provide no remote attestation | A trusted execution environment or equivalent |
| Mandatory networking with UDP, IPv6, or QUIC | VM data plane does not support these | Host execution with accepted cooperative-network limitations |
| Short interactive tasks you already watch | Existing approval mode costs little attention | Agent-native approval |

## Choosing

1. Is the main risk **workspace files**? Staging/selective apply fit directly.
2. Must you explain **which limits actually applied** afterward? Use the evidence model.
3. Need **one semantic model across agents or machines**? The common entry point fits; clusters remain a direction in the [trust ladder](trust-ladder.md).

If all answers are no, choose a lighter approach. See [comparisons](comparisons.md).
