"""B-SWE-RESIDENT-ENG: opt-in helpers for externally supervised replay.

No services, resource limits or samplers are started on import. The deployment
owns isolation policy and physical cleanup; these helpers do not replace them.
"""

import argparse
import asyncio
from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import subprocess
import sys


def checked_exec(check_script, check_args, command):
    """Run a trusted Python check, then replace this process on success only.

    Invoke inside the payload's systemd ExecStart sandbox, retaining all unit
    restrictions. The check child inherits that namespace, cwd and environment.
    Use an absolute script and executable; no shell parsing or PATH lookup.
    Check failure or exec failure propagates to the service manager. This does
    not emulate ExecStartPre privilege prefixes or separate pre-start policies.
    """
    if not Path(check_script).is_absolute():
        raise ValueError("check script must be absolute")
    if not command or not Path(command[0]).is_absolute():
        raise ValueError("payload executable must be absolute")
    subprocess.run([sys.executable, "-B", str(check_script), *check_args], check=True)
    os.execv(command[0], command)


class ResourceSampler:
    """Bound synchronous metric reads to a dedicated pool, not the tool pool.

    Use one async context per cohort and await each VM's read before submitting
    another; this bounds outstanding work to the number of observers. Exceptions
    propagate rather than becoming missing/zero samples. Stop observer tasks
    before leaving the context. Exit drains running reads off the event loop,
    even on cancellation, so VM cleanup cannot race those reads. A stuck kernel
    read still needs the deployment's external finite supervisor.
    """

    def __init__(self, workers=4):
        if not isinstance(workers, int) or isinstance(workers, bool) or workers < 1:
            raise ValueError("workers must be a positive integer")
        self.workers = workers
        self._pool = None
        self._used = False

    async def __aenter__(self):
        if self._used:
            raise RuntimeError("sampler contexts cannot be reused")
        self._used = True
        self._pool = ThreadPoolExecutor(self.workers, thread_name_prefix="replay-metrics")
        return self

    async def read(self, collect, *args):
        if self._pool is None:
            raise RuntimeError("sampler is not open")
        return await asyncio.get_running_loop().run_in_executor(self._pool, collect, *args)

    async def __aexit__(self, *exc):
        pool, self._pool = self._pool, None
        shutdown = asyncio.create_task(asyncio.to_thread(pool.shutdown, wait=True, cancel_futures=True))
        cancelled = False
        while True:
            try:
                await asyncio.shield(shutdown)
                break
            except asyncio.CancelledError:
                cancelled = True
        if cancelled:
            raise asyncio.CancelledError


def main():
    parser = argparse.ArgumentParser(description="Check isolation inside ExecStart, then exec the payload")
    parser.add_argument("--check-script", required=True)
    parser.add_argument("--check-arg", action="append", default=[])
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    checked_exec(args.check_script, args.check_arg, command)


if __name__ == "__main__":
    main()
