#!/usr/bin/env python3
"""Bounded CLI adapter for the Podman-derived Bats contracts (Apache-2.0)."""

import os
import signal
import subprocess
import sys


def command_prefix(environ):
    """Resolve pVisor's literal executable path; invoke its top-level container commands."""
    return [environ.get("PVISOR_BINARY", "pvisor")]


def main():
    try:
        prefix = command_prefix(os.environ)
        with subprocess.Popen(prefix + sys.argv[1:], start_new_session=True) as process:
            try:
                code = process.wait(timeout=120)
                return code if code >= 0 else 128 - code
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                print("container CLI timed out after 120 seconds", file=sys.stderr)
                return 124
    except OSError as error:
        print(error, file=sys.stderr)
        return 125


if __name__ == "__main__":
    sys.exit(main())
