"""Startup labels and expansion of explicit semaspec plans; no hidden subject argv."""

import re

NAMES = (
    "native",
    "pvisor-host",
    "pvisor-staged",
    "pvisor-vm",
    "docker",
    "fc-system",
    "qemu",
    "qemu-microvm",
)
LABELS = dict(
    zip(
        NAMES,
        (
            "Native",
            "pVisor host",
            "pVisor staged",
            "pVisor VM",
            "Docker",
            "Firecracker",
            "QEMU",
            "QEMU microvm",
        ),
    )
)


def timing_rows(document):
    """Map semaspec-validated subject plans to script-owned Ready/Exit rows."""
    plans = document["experiments"]
    if document["tables"] or not plans or any(cell["id"] not in NAMES for cell in plans):
        raise ValueError("expected known startup benchmark plans")
    return {
        LABELS[cell["id"]]: "$ python3 @REPO@/benchmark/startup/timing.py @BATCH@ --backend "
        + cell["id"]
        for cell in plans
    }


def subject(plan, variables):
    """Expand prepared paths in each argv element without invoking a shell."""
    argv = []
    for value in plan["argv"]:
        for name, replacement in variables.items():
            value = value.replace("@" + name + "@", str(replacement))
        if re.search(r"@[A-Z_]+@", value):
            raise ValueError("unresolved subject path")
        argv.append(value)
    return argv
