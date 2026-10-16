"""Connect the cache matrix to the CLI-compatible owned-inputs example.

Preparation only selects an already imported/sealed snapshot manifest. The
driver itself opens both SnapshotBase leases, runs the ordinary managed Job,
and publishes the receipt after completion. No live directory is reclassified
by this adapter.
"""

import hashlib
import json
from pathlib import Path


DRIVER_CONDITIONS = {
    "baseline": "baseline", "sealed-rootfs": "rootfs",
    "snapshot-workspace": "workspace", "both": "both",
}


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def prepare_trial(args, condition, trial):
    """Record the selected manifest outside the launch/Ready clocks."""
    values = args.cache_matrix_environment
    if values.get("PVISOR_FS_OWNED_INPUTS_CONDITION") != DRIVER_CONDITIONS[condition]:
        raise ValueError("condition mapping does not select the owned-inputs driver arm")
    manifest_path = Path(values["PVISOR_FS_OWNED_INPUTS_MANIFEST"]).resolve(strict=True)
    manifest = json.loads(manifest_path.read_text())
    if manifest.get("schema") != 1:
        raise ValueError("unsupported owned-inputs manifest")
    for key, path in (("source_rootfs", args.assets / "rootfs"),
                      ("source_workspace", args.assets / "rootfs/work")):
        if Path(manifest[key]).resolve(strict=True) != path.resolve(strict=True):
            raise ValueError(f"{key} does not match the reference prepared input")
    for name in ("rootfs", "workspace"):
        path = Path(manifest[f"{name}_owned"])
        if not path.is_absolute() or path.is_symlink() or not path.is_dir():
            raise ValueError(f"invalid owned {name} path")
        if not manifest.get(f"{name}_reference", {}).get("id"):
            raise ValueError(f"missing sealed {name} snapshot reference")
    trial_root = args.output / "trials" / f"filesystem-pvisor-vm-{trial:03d}"
    receipt = trial_root / "owned-inputs-receipt.json"
    return {
        "manifest": str(manifest_path), "manifest_sha256": digest(manifest_path),
        "rootfs_reference": manifest["rootfs_reference"],
        "workspace_reference": manifest["workspace_reference"],
        "rootfs_owned": manifest["rootfs_owned"],
        "workspace_owned": manifest["workspace_owned"],
        "receipt": str(receipt),
        "environment": {"PVISOR_FS_OWNED_INPUTS_RECEIPT": str(receipt)},
    }


def validate_trial(args, row, condition, trial, prepared):
    """Match observed runtime ownership to the exact manifest and Run Bundle."""
    root = Path(row["logs"])
    receipt_path = Path(prepared["receipt"])
    receipt = json.loads(receipt_path.read_text())
    if receipt.get("schema") != 1 or receipt.get("condition") != DRIVER_CONDITIONS[condition]:
        raise ValueError("wrong owned-inputs trial receipt")
    if digest(prepared["manifest"]) != prepared["manifest_sha256"]:
        raise ValueError("owned-inputs manifest changed during the trial")
    for name in ("manifest_sha256", "rootfs_reference", "workspace_reference",
                 "rootfs_owned", "workspace_owned"):
        if receipt.get(name) != prepared[name]:
            raise ValueError(f"owned-inputs receipt has a different {name}")
    if receipt.get("rootfs_lease") is not True or receipt.get("workspace_lease") is not True:
        raise ValueError("both SnapshotBase leases must survive the complete Job")
    if receipt.get("writeback") is not False or receipt.get("dax") is not False:
        raise ValueError("writeback and DAX must remain disabled")
    if receipt.get("runtime") != "PVisor/ManagedJob/Overlay/VmExecutor":
        raise ValueError("unexpected owned-inputs execution path")
    if receipt.get("cli_host_service") is not False:
        raise ValueError("driver must identify its direct runtime path")
    workspace = (root / "workspace").resolve(strict=True)
    if Path(receipt["workspace_apply_target"]).resolve(strict=True) != workspace:
        raise ValueError("apply target is not this trial's independent workspace")
    if workspace == Path(prepared["workspace_owned"]).resolve(strict=True):
        raise ValueError("apply target aliases the sealed workspace lower")
    snapshot = condition in ("snapshot-workspace", "both")
    if receipt.get("workspace_snapshot") is not snapshot:
        raise ValueError("workspace snapshot selection differs from the planned arm")
    if receipt.get("workspace_frozen_baseline") is not snapshot:
        raise ValueError("workspace frozen-baseline selection differs from the planned arm")
    expected_lower = Path(prepared["workspace_owned"]).resolve(strict=True) if snapshot else workspace
    if Path(receipt["workspace_lower"]).resolve(strict=True) != expected_lower:
        raise ValueError("actual workspace lower differs from the planned live/snapshot input")
    bundles = list((root / "runs").glob("*/run-bundle.json"))
    bundles += list((root / "stage").glob("run-bundle.json"))
    if len(bundles) != 1:
        raise ValueError("expected one retained Run Bundle")
    run = json.loads(bundles[0].read_text())["run"]
    if run["run_id"] != receipt.get("run_id") or receipt.get("exit_code") != 0:
        raise ValueError("receipt is not the completed successful Run")
    if run["state"] != "completed" or run["exit_code"] != 0:
        raise ValueError("owned-inputs Job was not completed successfully")
    return {
        "rootfs_immutable": receipt["rootfs_immutable"],
        "workspace_immutable": receipt["workspace_immutable"], "dax": receipt["dax"],
        "evidence": {"receipt": str(receipt_path), "receipt_sha256": digest(receipt_path),
                     "manifest": prepared["manifest"], "manifest_sha256": prepared["manifest_sha256"],
                     "bundle": str(bundles[0]), "snapshot_leases": "held through completed Job",
                     "workspace_lower": str(expected_lower),
                     "workspace_snapshot": snapshot, "workspace_frozen_baseline": snapshot,
                     "runtime": receipt["runtime"], "cli_host_service": False},
    }
