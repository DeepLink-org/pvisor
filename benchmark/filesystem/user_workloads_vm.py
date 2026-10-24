#!/usr/bin/env python3
"""Benchmark: B-FS-TOOLS, ordinary CLI VM launch descriptions for V2 workloads.

This module only describes the measured product command.  The shared user
benchmark launcher owns the fresh user/mount/PID namespace, private Host socket
namespace, prepared tool root, resource budget and the pre-exec launch marker.
No VM, build or measurement runs while importing this module.

The two configurations use the same product binaries and source-built firmware.
They differ only in guest writeback; host writeback remains disabled.  The normal
CLI OverlayFS device path disables DAX in product code rather than through an
invented environment setting.  A supplied directory rootfs has no authenticated
immutable-generation ownership declaration.
"""

from pathlib import Path

VM_BACKENDS = {
    "vm": 0,
    "vm-writeback": 1,
    "pvisor-vm": 0,
    "pvisor-vm-wb": 1,
}

VM_WORK_ROOT = "output"
VM_FIXTURE_ROOT = "input"
VM_ARTIFACT_ROOT = "/bench-artifacts"
VM_STAGE_ROOT = "/bench-trial/stage"


def vm_environment(backend):
    """Return explicit launch preferences for one registered VM configuration.

    These are Host-side preferences, inherited by the private CLI Host and VM
    child.  They are not claims that runtime negotiation was separately profiled;
    formal timing has instrumentation disabled.  Other product cache defaults
    are left to the identical frozen CLI, with no external caller overrides.
    """
    if backend not in VM_BACKENDS:
        raise ValueError("unknown ordinary CLI VM configuration: " + backend)
    return {
        "PVISOR_VM_FS_WRITEBACK": str(VM_BACKENDS[backend]),
        "PVISOR_VM_FS_HOST_WRITEBACK": "0",
        "PVISOR_FS_PROFILE": "0",
        "PVISOR_STARTUP_TIMING": "0",
        "PVISOR_STARTUP_PROFILE": "0",
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONPYCACHEPREFIX": "/__pvisor_reference_no_pyc__",
        "RUST_LOG": "off",
    }


def vm_payload_argv(case, backend, rounds=3, ram_mib=512):
    """Build the exact ordinary CLI command inside the shared prepared root.

    The caller must set its cwd to the fresh ``/bench-trial/work`` and install
    private HOME/XDG/Job paths.  The directory rootfs is the namespace's prepared
    root.  Relative output and input paths follow the workspace selected by the
    ordinary CLI.  The shared wrapper resolves both against that actual cwd
    before entering the static payload; reads and writes therefore traverse the
    selected workspace filesystem.  Ready is emitted before payload setup;
    process exit and Host cleanup remain outside Ready and payload phase clocks.
    """
    vm_environment(backend)  # Validate the named condition before building argv.
    if not isinstance(case, str) or not (
        len(case) == 3 and case[0] in "MRS" and case[1:].isdigit()
    ):
        raise ValueError("a registered workload case ID is required")
    if rounds != 3 or ram_mib != 512:
        raise ValueError("the shallow user protocol requires 3 rounds and 512 MiB")
    return [
        VM_ARTIFACT_ROOT + "/pvisor",
        "run",
        "--no-agent-defaults",
        "--pass-env",
        "PYTHONDONTWRITEBYTECODE",
        "--pass-env",
        "PYTHONPYCACHEPREFIX",
        "--overlaynet",
        "off",
        "--stdio",
        "inherit",
        "--timeout",
        "180s",
        "--vm",
        "--rootfs",
        "/",
        "--cpu",
        "2",
        "--memory",
        str(ram_mib) + "MiB",
        "--stage",
        VM_STAGE_ROOT,
        "--vm-library-dir",
        VM_ARTIFACT_ROOT,
        "--",
        VM_ARTIFACT_ROOT + "/payload",
        case,
        VM_WORK_ROOT,
        VM_FIXTURE_ROOT,
        str(rounds),
        str(ram_mib),
    ]


def vm_result_work_root(folder):
    """Locate retained guest output for an independent post-exit host oracle.

    The returned physical upper is not a merged namespace.  The shared harness
    must apply the existing strict, case-specific OverlayFS marker materializer
    before invoking the frozen payload's ``--verify`` operation.  Read-only
    cases may leave this path absent.
    """
    return Path(folder) / "stage" / "upper" / VM_WORK_ROOT


def vm_configuration(backend):
    """Return descriptive inputs; formal measurements are recorded elsewhere."""
    return {
        "executor": "ordinary pvisor CLI VM",
        "guest_vcpus": 2,
        "guest_memory_mib": 512,
        "requested_guest_writeback": VM_BACKENDS[backend],
        "requested_host_writeback": 0,
        "dax": False,
        "dax_source": "product OverlayFS VM device path disables DAX",
        "rootfs_ownership": "ordinary directory; no immutable-generation claim",
        "host_lifecycle": "fresh private Host per case/environment",
        "environment": vm_environment(backend),
    }
