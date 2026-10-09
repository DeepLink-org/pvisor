#!/usr/bin/env python3
"""Real Linux independent full-copy environment save/exit/restore on macOS HVF."""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--report", type=Path, default=ROOT / "target/vm-validation/environment-linux.json"
    )
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--guest-bin", type=Path, required=True)
    parser.add_argument("--firmware-dir", type=Path, required=True)
    args = parser.parse_args()
    driver = args.binary.resolve(strict=True)
    guest = args.guest_bin.resolve(strict=True)
    firmware = args.firmware_dir.resolve(strict=True)
    if not (firmware / "libkrunfw.5.dylib").is_file():
        raise RuntimeError("Set PVISOR_CASE_VM_LIBRARY_DIR to a local built firmware directory")
    with tempfile.TemporaryDirectory(prefix="pvisor-environment-snapshot-") as directory:
        base = Path(directory)
        runner = base / "vm-runner"
        shutil.copyfile(driver, runner)
        runner.chmod(0o700)
        root = base / "rootfs"
        root.mkdir()
        shutil.copyfile(guest, root / "init.krun")
        (root / "init.krun").chmod(0o700)
        # Include state the guest never looks up, to exercise the full seal.
        (root / "unvisited").write_bytes(b"independent filesystem contents")
        env = dict(os.environ, PVISOR_CASE_VM_LIBRARY_DIR=str(firmware.resolve()))
        source = subprocess.run(
            [str(runner), "save", str(base)], env=env, text=True, capture_output=True, timeout=60
        )
        print("SOURCE:", source.returncode, source.stdout, source.stderr, flush=True)
        if source.returncode != 0:
            raise RuntimeError("environment save failed")
        original = (root / "ready").read_text()
        identity = (base / "snapshot.id").read_text().strip()
        object_path = base / "store/objects" / identity
        manifest = json.loads((object_path / "manifest.json").read_bytes())
        machine = json.loads((object_path / "machine.json").read_bytes())
        # Source process has been reaped. Remove the entire source backing tree.
        shutil.rmtree(root)
        assert not root.exists()
        rejected = []
        for name in ["ram.bin", "machine.json", "manifest.json"]:
            path = object_path / name
            pristine = path.read_bytes()
            damaged = bytearray(pristine)
            damaged[0] ^= 1
            path.write_bytes(damaged)
            bad = subprocess.run(
                [str(runner), "restore", str(base)],
                env=env,
                text=True,
                capture_output=True,
                timeout=15,
            )
            assert bad.returncode != 0 and "digest mismatch" in bad.stderr, bad.stderr
            assert not (base / "restored-rootfs").exists()
            rejected.append(name)
            path.write_bytes(pristine)
        out = base / "restore.stdout"
        err = base / "restore.stderr"
        work = base / "restored-rootfs"
        with out.open("w") as stdout, err.open("w") as stderr:
            target = subprocess.Popen(
                [str(runner), "restore", str(base)], env=env, stdout=stdout, stderr=stderr
            )
            try:
                deadline = time.monotonic() + 40
                while not (work / "ready").exists() or (work / "ready").read_text() == original:
                    if target.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError(
                            "independent restore failed: " + out.read_text() + err.read_text()
                        )
                    time.sleep(0.05)
                resumed = (work / "ready").read_text()
                assert resumed.split()[:2] == original.split()[:2], (original, resumed)
                assert int(resumed.split()[2]) > int(original.split()[2])
                assert (work / "unvisited").read_bytes() == b"independent filesystem contents"
                assert not root.exists()
                contender = subprocess.run(
                    [str(runner), "restore", str(base)],
                    env=env,
                    text=True,
                    capture_output=True,
                    timeout=15,
                )
                assert contender.returncode != 0 and "another runner owns" in contender.stderr
                # VM has installed its state and owns a full private worktree:
                # removing the published object must not break its execution.
                deleted = subprocess.run(
                    [str(runner), "delete", str(base)],
                    env=env,
                    text=True,
                    capture_output=True,
                    timeout=15,
                )
                assert deleted.returncode == 0, deleted.stderr
                assert not object_path.exists()
                (work / "release").write_text("continue after backing deletion")
                while not (work / "result").exists():
                    if target.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError(
                            "guest result missing: " + out.read_text() + err.read_text()
                        )
                    time.sleep(0.05)
                result = (work / "result").read_text()
                assert result.startswith("linux-cold-restore-ok ")
                assert len((work / "starts").read_text().splitlines()) == 1
                record = {
                    "scope": "same-host/boot/build real Linux independent full-copy environment snapshot",
                    "snapshot_id": identity,
                    "cpu_count": len(machine["state"]["cpus"]),
                    "ram_mapping_bytes": sum(item["len"] for item in machine["state"]["ram"]),
                    "device_inventory": [
                        {
                            "base": item["base"],
                            "len": item["len"],
                            "kind": item["device"]["kind"],
                            "virtio_type": item["device"]["state"].get("device_type")
                            if item["device"]["kind"] == "Virtio"
                            else None,
                        }
                        for item in machine["state"]["devices"]
                    ],
                    "excluded_resources": machine["excluded_resources"],
                    "source_pid": machine["source_pid"],
                    "restore_pid": target.pid,
                    "source_reaped_before_restore": True,
                    "original_tree_deleted_before_restore": True,
                    "published_object_deleted_before_guest_final_check": True,
                    "guest_before": original,
                    "guest_after": resumed,
                    "result": result,
                    "manifest": manifest,
                    "negative_payloads": rejected,
                    "same_execution_lease_rejected_second_runner": True,
                    "source_stdout": source.stdout,
                    "source_stderr": source.stderr,
                    "restore_stdout": out.read_text(),
                    "restore_stderr": err.read_text(),
                    "driver_sha256": hashlib.sha256(runner.read_bytes()).hexdigest(),
                    "firmware_sha256": hashlib.sha256(
                        (firmware / "libkrunfw.5.dylib").read_bytes()
                    ).hexdigest(),
                    "guest_sha256": hashlib.sha256((work / "init.krun").read_bytes()).hexdigest(),
                }
                evidence = args.report
                evidence.parent.mkdir(parents=True, exist_ok=True)
                evidence.write_text(json.dumps(record, ensure_ascii=False, indent=2) + "\n")
                print(
                    json.dumps({"result": result, "evidence": str(evidence)}, ensure_ascii=False),
                    flush=True,
                )
            finally:
                if target.poll() is None:
                    target.terminate()
                    try:
                        target.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        target.kill()
                        target.wait()


if __name__ == "__main__":
    main()
