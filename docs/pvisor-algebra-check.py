#!/usr/bin/env python3
"""Finite checks for pvisor-algebra.md; not a runtime or a driver proof.

Run: python3 docs/pvisor-algebra-check.py
Uses finite values, explicit rewrite passes and small resource models.
Abort and Within model derived error/resource helpers, not core syntax.
"""

from dataclasses import dataclass
from itertools import combinations, product


@dataclass(frozen=True)
class Pure:
    value: int


@dataclass(frozen=True)
class Abort:
    reason: str


@dataclass(frozen=True)
class Call:
    operation: str
    continuations: tuple
    context: int = 0


@dataclass(frozen=True)
class Within:
    body: object
    continuations: tuple


IDENTITY = (Pure(0), Pure(1))


def bind(program, continuation):
    if isinstance(program, Pure):
        return continuation[program.value]
    if isinstance(program, Abort):
        return program
    mapped = tuple(bind(branch, continuation) for branch in program.continuations)
    if isinstance(program, Call):
        return Call(program.operation, mapped, program.context)
    if isinstance(program, Within):
        return Within(program.body, mapped)
    raise TypeError(program)


def within(program):
    return Within(program, IDENTITY)


def run(program, cell=0):
    """Result, parent-visible cell, trace; scope cleanup always succeeds here."""
    if isinstance(program, Pure):
        return program.value, cell, ()
    if isinstance(program, Abort):
        return None, cell, (("abort", program.reason),)
    if isinstance(program, Call):
        if program.operation == "read":
            result = cell
        elif program.operation == "write":
            result, cell = 1, 1
        else:
            raise ValueError(program.operation)
        value, cell, trace = run(program.continuations[result], cell)
        return value, cell, ((program.operation, result),) + trace
    if isinstance(program, Within):
        value, _, body_trace = run(program.body, 0)
        trace = (("enter",),) + body_trace + (("exit",),)
        if value is None:
            return None, cell, trace
        value, cell, rest = run(program.continuations[value], cell)
        return value, cell, trace + rest
    raise TypeError(program)


def check_composition():
    read = Call("read", IDENTITY)
    write = Call("write", IDENTITY)
    programs = (
        *IDENTITY,
        Abort("failure"),
        read,
        write,
        within(read),
        within(bind(write, (read, read))),
        bind(write, (Abort("after-write"),) * 2),
        within(bind(write, (Abort("in-scope"),) * 2)),
    )
    functions = tuple(product(programs, repeat=2))
    for f in functions:
        for value in (0, 1):
            assert bind(Pure(value), f) == f[value]
    for p in programs:
        assert bind(p, IDENTITY) == p
        for f, g in product(functions, repeat=2):
            left = bind(bind(p, f), g)
            right = bind(p, tuple(bind(branch, g) for branch in f))
            assert left == right
    # A new scope is not a bind-preserving transformation.
    shared = within(bind(write, (read, read)))
    separate = bind(within(write), (within(read),) * 2)
    assert run(shared)[0] == 1
    assert run(separate)[0] == 0
    # A later failure does not erase an earlier write or its evidence.
    result, cell, trace = run(programs[-2])
    assert result is None and cell == 1 and trace[0] == ("write", 1)
    # Private scope cleanup restores the parent's view, but retains its trace.
    result, cell, trace = run(programs[-1])
    assert result is None and cell == 0
    assert ("write", 1) in trace and trace[-1] == ("exit",)


def subsets(values):
    return tuple(
        frozenset(items)
        for size in range(len(values) + 1)
        for items in combinations(values, size)
    )


def check_policy_and_projection():
    # Finite canonical action universe, not raw path-string comparison.
    actions = ("local:read", "local:write", "remote:write")
    policies = subsets(actions)
    for a, b, c in product(policies, repeat=3):
        assert a & b <= a
        assert a & b == b & a
        assert (a & b) & c == a & (b & c)
        assert a & a == a
    events = ((1, "read"), (2, "write"), (3, "read"))
    for a, b in product(subsets((1, 2, 3)), repeat=2):
        select = lambda log, ids: tuple(e for e in log if e[0] in ids)
        assert select(select(events, b), a) == select(events, a & b)
        assert select(select(events, a), a) == select(events, a)


def check_dispatch_protocol():
    # Exhaust all paths in a finite abstraction. The bool is the durable
    # precondition; count is target dispatches, not journal writes.
    for allowed, intent_committed in product((False, True), repeat=2):
        start = ("requested", 0)
        pending = [start]
        visited = set()
        while pending:
            state, count = pending.pop()
            if (state, count) in visited:
                continue
            visited.add((state, count))
            if state == "requested":
                pending.append(("authorized" if allowed else "denied", count))
            elif state == "authorized":
                if intent_committed:
                    pending.append(("in-flight", count + 1))
                else:
                    pending.append(("audit-failed", count))
            elif state == "in-flight":
                pending.extend((outcome, count) for outcome in ("ok", "error", "unknown"))
            assert count <= 1
            if not allowed or not intent_committed:
                assert count == 0
            if state == "unknown":
                assert count == 1  # No automatic second dispatch.


def restore(log):
    """Tiny state-changing record decoder: contiguous sequence + stable identity."""
    state, expected, seen = 0, 0, {}
    for identity, sequence, value in log:
        content = (sequence, value)
        if identity in seen:
            if seen[identity] != content:
                raise ValueError("identity conflict")
            continue
        if sequence != expected or value is None:
            raise ValueError("incomplete trace")
        seen[identity] = content
        state, expected = value, expected + 1
    return state


def replay(requests, tape):
    """No external I/O. This checks matching, not an actual process replay."""
    if len(requests) != len(tape):
        raise ValueError("incomplete or unconsumed tape")
    outcomes = []
    for request, (recorded_request, outcome) in zip(requests, tape):
        if request != recorded_request:
            raise ValueError("replay divergence")
        outcomes.append(outcome)
    return outcomes


def rejects(function, *args):
    try:
        function(*args)
    except ValueError:
        return
    raise AssertionError("expected explicit rejection")


def check_restore_and_replay():
    first, second = ("e1", 0, 1), ("e2", 1, 2)
    assert restore((first, second)) == restore((first, first, second)) == 2
    rejects(restore, (second,))
    rejects(restore, (first, ("e1", 0, 2)))
    rejects(restore, (first, ("e2", 1, None)))
    request = ("fs.read@1", "context-1@1", "resource-1", 0, 8)
    tape = ((request, b"original"),)
    assert replay((request,), tape) == [b"original"]
    for index in range(len(request)):
        changed = list(request)
        changed[index] = "different"
        rejects(replay, (tuple(changed),), tape)
    rejects(replay, (), tape)
    rejects(replay, (request, request), tape)


@dataclass(frozen=True)
class Reply:
    outcome: str


@dataclass(frozen=True)
class Redirect:
    target: str


def rewrite_request(request, passes, allowed, trace, calls):
    """A replacement enters only later passes; dispatch always checks access."""
    trace.append(("requested", request))
    if not allowed("admit", request):
        return "denied"
    for index, rules in enumerate(passes):
        for name, matches, replacement in rules:
            if not matches(request):
                continue
            if not allowed("rewrite", name):
                return "denied"  # Do not try a lower-priority rule.
            trace.append(("rewritten", request, name))
            if isinstance(replacement, Reply):
                outcome = replacement.outcome
            elif isinstance(replacement, Redirect):
                outcome = rewrite_request(
                    replacement.target, passes[index + 1:], allowed, trace, calls
                )
            else:
                raise TypeError(replacement)
            if not allowed("reply", name):
                return "denied"
            trace.append(("policy-result", outcome))
            return outcome
    if not allowed("dispatch", request):
        return "denied"
    calls.append(request)
    trace.append(("driver-result", request))
    return "succeeded"


def check_rewriting():
    mock = ("mock", lambda request: request == "local", Reply("succeeded"))
    deny = ("deny", lambda request: request == "local", Reply("denied"))
    remote = ("remote", lambda request: request == "local", Redirect("remote"))
    allow = lambda _action, _target: True

    def run_rules(rules, guard=allow, later=()):
        trace, calls = [], []
        outcome = rewrite_request("local", (rules, *later), guard, trace, calls)
        return outcome, trace, calls

    result, trace, calls = run_rules((mock, deny))
    assert result == "succeeded" and not calls
    assert ("policy-result", "succeeded") in trace
    assert run_rules((deny, mock))[0] == "denied"  # Order matters.
    assert not run_rules((deny, mock))[2]
    assert run_rules(())[2] == ["local"]
    assert run_rules((remote,))[2] == ["remote"]  # Original is never dispatched.
    result, trace, calls = run_rules((remote,), lambda action, _target: action != "reply")
    assert result == "denied" and calls == ["remote"]  # Prior effects remain visible.
    assert ("driver-result", "remote") in trace
    assert run_rules((remote,), lambda action, target: (action, target) != ("dispatch", "remote"))[2] == []
    assert run_rules((remote,), lambda action, target: (action, target) != ("admit", "remote"))[0] == "denied"
    assert run_rules((mock,), lambda action, _target: action != "dispatch")[0] == "succeeded"
    assert run_rules((mock,), lambda action, _target: action != "admit")[0] == "denied"
    assert run_rules((mock,), lambda action, _target: action != "reply")[0] == "denied"
    assert run_rules((mock, remote), lambda action, target: (action, target) != ("rewrite", "mock"))[2] == []
    same_target = (("same", lambda _request: True, Redirect("local")),)
    result, trace, calls = run_rules(same_target)
    assert result == "succeeded" and calls == ["local"]
    assert sum(event[0] == "rewritten" for event in trace) == 1
    result, trace, calls = run_rules(same_target, later=(same_target,) * 2)
    assert calls == ["local"]
    assert sum(event[0] == "rewritten" for event in trace) == 3
    remote_mock = ("remote-mock", lambda request: request == "remote", Reply("succeeded"))
    assert run_rules((remote, remote_mock))[2] == ["remote"]
    assert run_rules((remote,), later=((remote_mock,),))[2] == []
    assert run_rules((remote_mock,), later=((remote,),))[2] == ["remote"]


def check_backend_contracts():
    # Binding fixes instance, implementation revision, operation contract and resource.
    local = ("local", "v1", "read@1", "file-a")
    remote = ("remote", "v2", "read@1", "file-b")
    registry = {"local": ("v1", frozenset({"read@1"}))}

    def dispatch(bound, backends, allowed, calls):
        instance, revision, contract, resource = bound
        descriptor = backends.get(instance)
        if descriptor is None or contract not in descriptor[1]:
            return "unsupported"
        if revision != descriptor[0]:
            return "stale-binding"
        if not allowed(instance, resource):
            return "denied"
        calls.append(bound)  # Exactly the validated immutable binding.
        return "succeeded"

    allow = lambda *_: True
    calls = []
    assert dispatch(local, registry, allow, calls) == "succeeded"
    original_calls = calls.copy()
    registry["remote"] = ("v2", frozenset({"read@1"}))
    calls.clear()
    assert dispatch(local, registry, allow, calls) == "succeeded"
    assert calls == original_calls  # Installing an unused backend changes nothing.
    calls.clear()
    assert dispatch(remote, registry, allow, calls) == "succeeded"
    assert calls == [remote]
    for binding, guard, expected in (
        (("remote", "v2", "write@1", "file-b"), allow, "unsupported"),
        (("missing", "v1", "read@1", "file-a"), allow, "unsupported"),
        (("remote", "v1", "read@1", "file-b"), allow, "stale-binding"),
        (remote, lambda instance, _: instance == "local", "denied"),
    ):
        calls.clear()
        assert dispatch(binding, registry, guard, calls) == expected
        assert not calls  # Neither dispatch nor fallback to another backend.

    # A backend expansion can create requests after an earlier mandatory transform.
    redact = ("redact", lambda req: req == "http:secret", Redirect("http:redacted"))
    lower = ("lower", lambda req: req == "llm:secret", Redirect("http:secret"))
    wrong_order = ((redact,), (lower,))
    trace, calls = [], []
    assert rewrite_request("llm:secret", wrong_order, allow, trace, calls) == "succeeded"
    assert calls == ["http:secret"]  # Counterexample: finite passes alone are insufficient.
    enforced = lambda action, req: action != "dispatch" or req != "http:secret"
    trace, calls = [], []
    assert rewrite_request("llm:secret", wrong_order, enforced, trace, calls) == "denied"
    assert not calls
    trace, calls = [], []
    assert rewrite_request("llm:secret", ((lower,), (redact,)), enforced, trace, calls) == "succeeded"
    assert calls == ["http:redacted"]


def check_agent_and_backend_views():
    # Agent observes a file view; the host also observes its storage and control work.
    def write_then_read(overlay):
        base, upper = 0, None
        events = [("agent", "write-request", 1)]
        if overlay:
            upper = base
            events.append(("host", "copy-up", base))
            upper = 1
        else:
            base = 1
        events.append(("host", "write-target", "upper" if overlay else "base"))
        events.append(("agent", "write-result", 1))
        visible = base if upper is None else upper
        events.extend((("agent", "read-result", visible), ("host", "audit-commit", 1)))
        return base, visible, events

    local, overlay = write_then_read(False), write_then_read(True)
    agent_events = lambda result: tuple(e for e in result[2] if e[0] == "agent")
    assert local[1] == overlay[1] == 1
    assert agent_events(local) == agent_events(overlay)
    assert local[0] == 1 and overlay[0] == 0
    assert local[2] != overlay[2]  # Internal differences stay in the host evidence.
    # A bare success reply without any modeled write doesn't implement this file view.
    mock_read_after_unmodeled_write = 0
    assert mock_read_after_unmodeled_write != overlay[1]


def check_local_context():
    # Program = Context -> Term; local rebinds configuration, allocates nothing.
    pure = lambda value: lambda _ctx: Pure(value)
    bind_p = lambda p, f: lambda ctx: bind(p(ctx), tuple(f(x)(ctx) for x in (0, 1)))
    local = lambda f, p: lambda ctx: p(f(ctx))
    read = lambda ctx: Call("read", IDENTITY, ctx)
    patches = (lambda c: c, lambda c: 1 - c, lambda _c: 0, lambda _c: 1)
    for ctx, f, g in product((0, 1), patches, patches):
        assert local(lambda c: c, read)(ctx) == read(ctx)
        assert local(f, local(g, read))(ctx) == local(lambda c: g(f(c)), read)(ctx)
        assert local(f, pure(1))(ctx) == pure(1)(ctx)
        k = lambda x: lambda c: Call("write", (Pure(x), Pure(x)), c)
        assert local(f, bind_p(read, k))(ctx) == bind_p(local(f, read), lambda x: local(f, k(x)))(ctx)
        exited = bind_p(local(f, read), lambda _x: read)(ctx)
        assert exited.context == f(ctx)
        assert all(branch.context == ctx for branch in exited.continuations)


def check_reordering():
    # Two private cells, no external observer; result labels retain operation identity.
    def execute(ops, initial):
        state, values, trace = dict(initial), {}, []
        for identity, kind, key, value in ops:
            if kind == "fail":
                return "failed", state, values, trace + [(identity, "failed")]
            if kind == "write":
                state[key] = value
            values[identity] = state[key]
            trace.append((identity, values[identity]))
        return "succeeded", state, values, trace

    read_a = ("read-a", "read", "a", None)
    read_b = ("read-b", "read", "b", None)
    write_a = ("write-a", "write", "a", 1)
    for a, b in product((0, 1), repeat=2):
        initial = {"a": a, "b": b}
        forward = execute((read_a, read_b), initial)
        reverse = execute((read_b, read_a), initial)
        assert forward[:3] == reverse[:3]  # Declared observation ignores independent order.
        assert sorted(forward[3]) == sorted(reverse[3])
        assert forward[3] != reverse[3]  # A fully ordered audit view is NOT equivalent.
    initial = {"a": 0, "b": 0}
    assert execute((read_a, write_a), initial)[:3] != execute((write_a, read_a), initial)[:3]
    # Distinct paths can alias the same resource; this case uses its canonical identity.
    alias_read = ("alias-read", "read", "a", None)
    assert execute((alias_read, write_a), initial)[:3] != execute((write_a, alias_read), initial)[:3]
    fail_b = ("fail-b", "fail", "b", None)
    before = execute((fail_b, write_a), initial)
    after = execute((write_a, fail_b), initial)
    assert before[0] == after[0] == "failed"
    assert before[1]["a"] == 0 and after[1]["a"] == 1  # Disjoint resources aren't sufficient.


@dataclass(frozen=True)
class PauseState:
    reasons: frozenset
    confirmed: frozenset
    recovery: frozenset = frozenset()
    running: object = False  # True / False / None (unknown).


def acquire_pause(state, token, acknowledged):
    if token in state.reasons:
        raise ValueError("duplicate lease")
    reasons = state.reasons | {token}
    if not acknowledged:
        return PauseState(reasons, state.confirmed, state.recovery | {token}, None)
    return PauseState(reasons, state.confirmed | {token}, state.recovery, False)


def settle_pause(state, token, exit_kind, mode, response_ready, resume_ack=True):
    """Host-controller model; no actual VM or distributed/crash guarantee."""
    if token not in state.confirmed or token in state.recovery:
        raise ValueError("pause not confirmed or owned by recovery")
    if not response_ready or (exit_kind != "success" and mode == "hold"):
        # Transfer ownership to recovery, preserving the pause reason.
        return PauseState(
            state.reasons, state.confirmed, state.recovery | {token}, False
        ), False
    remaining = state.reasons - {token}
    resume_requested = not remaining
    running = (True if resume_ack else None) if resume_requested else False
    return PauseState(
        remaining, state.confirmed - {token}, state.recovery - {token}, running
    ), resume_requested


def check_pause_leases():
    for others in subsets(("admin", "other-operation")):
        initial = PauseState(others, others, running=not others)
        paused = acquire_pause(initial, "ours", True)
        assert paused.running is False and "ours" in paused.confirmed
        for exit_kind, mode, ready in product(
            ("success", "failure", "cancelled"), ("hold", "resume-error"), (False, True)
        ):
            settled, resume = settle_pause(paused, "ours", exit_kind, mode, ready)
            assert others <= settled.reasons
            if others:
                assert not resume and settled.running is False
            if not ready or (exit_kind != "success" and mode == "hold"):
                assert "ours" in settled.recovery and not resume
                rejects(settle_pause, settled, "ours", "success", "hold", True)
            elif not others:
                assert resume and settled.running is True
        uncertain = acquire_pause(initial, "ours", False)
        assert "ours" not in uncertain.confirmed and uncertain.running is None
        rejects(settle_pause, uncertain, "ours", "success", "hold", True)
    paused = acquire_pause(PauseState(frozenset(), frozenset(), running=True), "ours", True)
    settled, resume = settle_pause(paused, "ours", "success", "hold", True, resume_ack=False)
    assert resume and settled.running is None  # Request is not confirmation.


if __name__ == "__main__":
    check_composition()
    check_policy_and_projection()
    check_dispatch_protocol()
    check_restore_and_replay()
    check_rewriting()
    check_backend_contracts()
    check_agent_and_backend_views()
    check_pause_leases()
    check_local_context()
    check_reordering()
    print("PASS: finite composition, context, rewrites, backend binding/views, final checks, reordering, replay and pause checks")
