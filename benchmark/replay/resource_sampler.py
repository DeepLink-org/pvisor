"""Dedicated bounded resource reads. No tool execution uses this executor."""

import asyncio
import time
from concurrent.futures import ThreadPoolExecutor


class ResourceSampler:
    def __init__(self, workers=4):
        if not isinstance(workers, int) or isinstance(workers, bool) or workers < 1:
            raise ValueError("workers must be a positive integer")
        self.workers = workers
        self.pool = None
        self.used = False
        self.failure = None

    async def __aenter__(self):
        if self.used:
            raise RuntimeError("sampler cannot be reused")
        self.used = True
        self.pool = ThreadPoolExecutor(self.workers, thread_name_prefix="resource-sampler")
        return self

    def check(self):
        if self.failure is not None:
            raise RuntimeError("resource sampling failed") from self.failure

    async def read(self, collect, *args):
        if self.pool is None:
            raise RuntimeError("sampler is closed")
        self.check()
        future = asyncio.get_running_loop().run_in_executor(self.pool, collect, *args)
        cancelled = False
        while True:
            try:
                result = await asyncio.shield(future)
                break
            except asyncio.CancelledError:
                # A kernel read cannot be killed. Drain it before VM teardown.
                cancelled = True
            except BaseException as exc:
                self.failure = exc
                if cancelled:
                    raise asyncio.CancelledError from exc
                raise
        if cancelled:
            raise asyncio.CancelledError
        return result

    async def __aexit__(self, *exc):
        pool, self.pool = self.pool, None
        closing = asyncio.create_task(
            asyncio.to_thread(pool.shutdown, wait=True, cancel_futures=True)
        )
        cancelled = False
        while True:
            try:
                await asyncio.shield(closing)
                break
            except asyncio.CancelledError:
                cancelled = True
        if cancelled:
            raise asyncio.CancelledError


def collect_to_log(collect, record, group_name, log_path, submitted_ns, due_ns):
    """Both metric reads and sample serialization/write happen in the worker."""
    import json

    started = time.monotonic_ns()
    row = collect(record, group_name)
    finished = time.monotonic_ns()
    row["sampling"] = dict(
        submitted_monotonic_ns=submitted_ns,
        started_monotonic_ns=started,
        finished_monotonic_ns=finished,
        queue_s=(started - submitted_ns) / 1e9,
        read_s=(finished - started) / 1e9,
        schedule_lag_s=max(0, submitted_ns - due_ns) / 1e9,
        executor="dedicated-resource-pool",
    )
    with log_path.open("a") as f:
        log_path.chmod(0o644)
        f.write(json.dumps(row) + "\n")
