"""Library path, experiment identity and frozen userspace checks."""

import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from library_compare import verify_inputs
from library_evidence import file_mapping_stats
from library_protocol import (
    VARIANTS,
    command,
    details,
    environment,
    validate_docker_views,
    validate_library,
)
from prepare_pool import tree_manifest
from scanner_compare import digest, save


class LibraryProtocolTests(unittest.TestCase):
    def test_docker_overlay_view_proofs_do_not_invent_cross_view_slot_counts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "libraries").mkdir()
            library = root / "libraries/libpayload.so"
            with library.open("wb") as stream:
                stream.truncate(64 * 1024**2 + 4096)
            slots = [[n, (n + 1) * 4096] for n in range(16384)]
            cfg = dict(inputs=str(root), hashes={"1:0": "payload-digest"})
            phase = dict(
                guest_pids=[1, 2, 3, 4],
                processes=[dict(pid=n, starttime=str(n)) for n in range(1, 5)],
            )
            observation = dict(
                schema="pvisor-docker-library-views/v1",
                shared_target_pages_all_processes=None,
                processes=[],
            )
            for pid in range(1, 5):
                device = 100 + pid
                text = (
                    f"10000000-14001000 r--p 00000000 00:{device:x} 42 /libraries/libpayload.so\n"
                    "Rss: 65540 kB\nPss: 16385 kB\nShared_Clean: 65540 kB\n"
                )
                paths = [
                    root / f"docker-library-{pid}-{part}.smaps" for part in ("before", "after")
                ]
                for path in paths:
                    path.write_text(text)
                proof = dict(
                    schema="pvisor-library-file-sharing/v2",
                    target_pages=16384,
                    payload_sha256="payload-digest",
                    maps_device=device,
                    inode=42,
                    mapping_path="/libraries/libpayload.so",
                    processes=[
                        dict(
                            pid=pid,
                            starttime=str(pid),
                            payload_pages=16384,
                            payload_index_and_file_offset=slots,
                        )
                    ],
                    shared_pages_all_processes=16384,
                    shared_payload_index_and_file_offset=slots,
                )
                stats = file_mapping_stats(text, (device, 42), "/libraries/libpayload.so")
                observation["processes"].append(
                    dict(
                        pid=pid,
                        source_sha256=digest(library),
                        target=proof,
                        smaps_sha256={path.name: digest(path) for path in paths},
                        accounting_before=stats,
                        accounting_after=stats,
                    )
                )
            validate_docker_views(observation, cfg, root, phase)
            observation["shared_target_pages_all_processes"] = 16384
            with self.assertRaisesRegex(ValueError, "falsely equated"):
                validate_docker_views(observation, cfg, root, phase)
            observation["shared_target_pages_all_processes"] = None
            view = observation["processes"][0]
            view["target"]["maps_device"] = 999
            with self.assertRaises(ValueError):
                validate_docker_views(observation, cfg, root, phase)
            view["target"]["maps_device"] = 101
            (root / "docker-library-1-before.smaps").write_text("forged")
            with self.assertRaisesRegex(ValueError, "changed"):
                validate_docker_views(observation, cfg, root, phase)

    def cfg(self, root, variant):
        return dict(
            root=str(root),
            inputs=str(root / "inputs"),
            library_variant=variant,
            arm=VARIANTS[variant][0],
            pattern="random-shared",
        )

    def test_same_binary_explicit_hint_controls_and_separate_dax(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            off = self.cfg(root, "pvisor-rootfs-physical-no-hints")
            on = self.cfg(root, "pvisor-rootfs-physical-hints")
            self.assertEqual(
                command(off, 1, root / "pool.sock"), command(on, 1, root / "pool.sock")
            )
            argv = command(on, 1, root / "pool.sock")
            self.assertIn("--rootfs-library", argv)
            self.assertNotIn("--libraries", argv)
            with patch.dict(os.environ, {"PVISOR_MEMORY_FILE_HINTS": "wrong"}):
                self.assertEqual(environment(off)["PVISOR_MEMORY_FILE_HINTS"], "0")
                self.assertEqual(environment(on)["PVISOR_MEMORY_FILE_HINTS"], "1")
                dax = self.cfg(root, "pvisor-dax")
                self.assertNotIn("PVISOR_MEMORY_FILE_HINTS", environment(dax))
            argv = command(dax, 1, None)
            self.assertIn("--libraries", argv)
            self.assertNotIn("--rootfs-library", argv)
            self.assertNotIn("--pool-socket", argv)
            self.assertEqual(argv[argv.index("--mode") + 1], "off")

    def test_firecracker_common_readonly_userspace_and_filter(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for variant, binary in [("firecracker-block", "firecracker")]:
                cfg = self.cfg(root, variant)
                argv = command(cfg, 2, None)
                self.assertEqual(argv[0], str(root / "inputs" / binary))
                self.assertEqual(
                    argv[-2:], ["--seccomp-filter", str(root / "inputs/firecracker-filter.bpf")]
                )
                boot = json.loads((root / "fc2.json").read_text())
                self.assertTrue(boot["drives"][0]["is_read_only"])
                self.assertEqual(
                    boot["drives"][0]["path_on_host"], str(root / "inputs/library.ext4")
                )
                self.assertIn(
                    "init=/usr/bin/python3 -- -B -u /library-worker.py random-shared 2 block",
                    boot["boot-source"]["boot_args"],
                )

    def test_variant_cannot_mislabel_arm_or_payload(self):
        cfg = self.cfg(Path("/tmp"), "pvisor-rootfs-physical-hints")
        for change in ({"arm": "pvisor-off"}, {"pattern": "random-unique"}):
            with self.assertRaisesRegex(ValueError, "mismatch"):
                details({**cfg, **change})

    def test_userspace_byte_mode_and_link_mutations_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "python-root/libraries").mkdir(parents=True)
            (root / "libraries").mkdir()
            (root / "libraries/libpayload.so").write_bytes(b"library")
            (root / "python-root/libraries/libpayload.so").write_bytes(b"library")
            (root / "library_worker.py").write_bytes(b"worker")
            (root / "python-root/library-worker.py").write_bytes(b"worker")
            alias = root / "python-root/alias"
            alias.symlink_to("library-worker.py")
            save(root / "python-root-manifest.json", tree_manifest(root / "python-root"))
            receipt = dict(
                hashes={
                    name: digest(root / name)
                    for name in (
                        "libraries/libpayload.so",
                        "library_worker.py",
                        "python-root-manifest.json",
                    )
                }
            )
            verify_inputs(root, receipt)
            worker = root / "python-root/library-worker.py"
            worker.write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "manifest"):
                verify_inputs(root, receipt)
            worker.write_bytes(b"worker")
            mode = worker.stat().st_mode & 0o7777
            worker.chmod(mode ^ 0o100)
            with self.assertRaisesRegex(ValueError, "manifest"):
                verify_inputs(root, receipt)
            worker.chmod(mode)
            alias.unlink()
            alias.symlink_to("libraries/libpayload.so")
            with self.assertRaisesRegex(ValueError, "manifest"):
                verify_inputs(root, receipt)
            alias.unlink()
            alias.symlink_to("library-worker.py")
            userspace = root / "python-root"
            userspace.chmod((userspace.stat().st_mode & 0o7777) ^ 0o002)
            with self.assertRaisesRegex(ValueError, "manifest"):
                verify_inputs(root, receipt)

    def test_target_evidence_requires_same_slot_all_vms_and_actual_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cfg = self.cfg(root, "pvisor-rootfs-physical-hints")
            cfg["hashes"] = {"1:0": "digest"}
            for n in range(1, 5):
                (root / f"vm{n}.stdout").write_text(
                    "LIBRARY_MODE rootfs statx_attributes 0 statx_attributes_mask 0\n"
                )
            sharing = dict(
                schema="pvisor-library-file-sharing/v2",
                target_pages=16384,
                payload_sha256="digest",
                maps_device=1,
                inode=2,
                mapping_path="/memfd:controlled",
                shared_pages_all_processes=1,
                shared_payload_index_and_file_offset=[[0, 4096]],
                processes=[
                    dict(
                        pid=n,
                        starttime=str(n),
                        payload_pages=1,
                        payload_index_and_file_offset=[[0, 4096]],
                    )
                    for n in range(1, 5)
                ],
            )
            evidence = dict(
                status="passed",
                variant=cfg["library_variant"],
                payload_sha256="digest",
                sharing=sharing,
                observed_start_ns=2,
                observed_end_ns=3,
                observed_hint_settings=[dict(pid=n, file_hints="1") for n in range(1, 5)],
            )
            raw = dict(
                phases=[
                    dict(
                        name="idle-60",
                        guest_pids=list(range(1, 5)),
                        processes=[dict(pid=n, starttime=str(n)) for n in range(1, 5)],
                    ),
                    dict(name="readback", monotonic_ns=0),
                    dict(name="cow-25", monotonic_ns=5),
                ]
            )

            def store():
                save(root / "library-evidence.json", evidence)
                raw["library_audit"] = dict(
                    status="passed",
                    started_ns=1,
                    finished_ns=4,
                    evidence_sha256=digest(root / "library-evidence.json"),
                )

            store()
            validate_library(raw, cfg, root)
            raw["library_audit"]["started_ns"] = -1
            with self.assertRaisesRegex(ValueError, "overlaps"):
                validate_library(raw, cfg, root)
            store()
            sharing["processes"][0]["payload_index_and_file_offset"] = [[0, 8192]]
            store()
            with self.assertRaisesRegex(ValueError, "intersection"):
                validate_library(raw, cfg, root)
            sharing["shared_pages_all_processes"] = 0
            sharing["shared_payload_index_and_file_offset"] = []
            store()
            validate_library(
                raw, cfg, root
            )  # zero cross-VM benefit remains a valid physical sample
            (root / "vm1.stdout").write_text("LIBRARY_MODE dax statx_attributes 2097152\n")
            with self.assertRaisesRegex(ValueError, "data path"):
                validate_library(raw, cfg, root)


if __name__ == "__main__":
    unittest.main()
