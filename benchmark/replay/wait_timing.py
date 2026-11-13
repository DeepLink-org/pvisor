"""Separate maintenance, sleep and event-loop dispatch; never shorten source waits."""

import asyncio
import time


async def measured_wait(backend, seconds, op, call, emit):
    async def maintenance(name, fn):
        emit(op, name + "_submitted")

        def invoke():
            emit(op, name + "_worker_begin")
            try:
                result = fn()
            except BaseException as exc:
                emit(op, name + "_worker_error", error=repr(exc))
                raise
            emit(op, name + "_worker_end")
            return result

        await call(invoke)
        emit(op, name + "_returned")

    await maintenance("pause", backend.enter_wait)
    start = time.monotonic_ns()
    deadline = start + int(seconds * 1e9)
    emit(op, "sleep_begin", planned_seconds=seconds, deadline_monotonic_ns=deadline)
    await asyncio.sleep(seconds)
    now = time.monotonic_ns()
    emit(
        op, "sleep_returned", elapsed_s=(now - start) / 1e9, wake_lag_s=max(0, now - deadline) / 1e9
    )
    await maintenance("resume", backend.leave_wait)


async def monitor_loop(sampler, path, interval=0.5):
    import json

    def write(row):
        with path.open("a") as f:
            path.chmod(0o644)
            f.write(json.dumps(row) + "\n")

    while True:
        due = time.monotonic_ns() + int(interval * 1e9)
        await asyncio.sleep(interval)
        now = time.monotonic_ns()
        await sampler.read(
            write,
            dict(
                unix_ns=time.time_ns(),
                monotonic_ns=now,
                expected_monotonic_ns=due,
                lag_s=max(0, now - due) / 1e9,
            ),
        )
