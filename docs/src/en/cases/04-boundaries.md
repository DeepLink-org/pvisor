# 4. Set boundaries for untrusted work

Move from file staging to execution isolation. --safe requires an available isolation implementation and retains workspace edits. Declare network and path policies, then inspect the actual evidence in the Run Bundle. Tests use a private temporary HOME and XDG directories to avoid personal configuration; check production filesystem capabilities separately.

### S-USE-012: Protect files outside the workspace

An untrusted script may write absolute paths. Use safe and verify outside files are unchanged rather than assuming staging proves isolation.

**Contract**: Workspace writes are staged, the original outside file is invisible and retains sentinel on the host; private /tmp writes do not reach the host, and evidence reports filesystem_non_bypassable.

**Violation**: An outside file is overwritten or evidence claims a boundary that was not enforced.

Execution and assertions: [Scrut S-USE-012](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/isolation/S-USE-012.md).

### S-USE-013: Deny access to sensitive paths

Credentials may be inside the workspace. Protect matching paths with --access and verify the task receives a refusal.

**Contract**: The Agent cannot read secret.txt but can write public.txt; the host secret remains unchanged.

**Violation**: The deny rule logs an event but returns the secret.

Execution and assertions: [Scrut S-USE-013](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/isolation/S-USE-013.md).

### S-USE-014: Deny networking and test the refusal

Use deny-all for offline work that needs no API. Test against a local listener to avoid reliance on a public service.

**Contract**: The host can reach the listener; the deny-all Session cannot, and network evidence is non_bypassable.

**Violation**: Network isolation silently degrades and the Agent reaches the host listener.

Execution and assertions: [Scrut S-USE-014](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/isolation/S-USE-014.md).

[Learning path](index.md) · [5. Replay trajectories and choose restoration tools](05-tools-and-restoration.md)
