"""Fixed JSON subjects, fresh trial evidence and paired document data publication."""

import csv
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "startup"))
SPEC = importlib.util.spec_from_file_location("startup_prepare", ROOT / "startup/common.py")
STARTUP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STARTUP)
from hooks import archive, prepare, validate, validate_output  # noqa: E402
from public_assets import csv_path as public_csv_path  # noqa: E402 - import after module-loading setup
from publish import compact_configuration, publish_startup  # noqa: E402

OUTPUT = 'REFERENCE_READY\nREFERENCE_RESULT {"mode":"ready","correctness":"passed"}\n'
SEMSPEC = Path(os.environ.get("SEMSPEC_BIN", ROOT.parent / "target/semspec/debug/semspec"))


class StartupContracts(unittest.TestCase):
    def test_public_configuration_preserves_identities_without_full_local_receipts(self):
        config = {
            "cpu_affinity": [0, 1],
            "guest_cpus": 2,
            "guest_memory_mib": 128,
            "device_transport": "virtio-mmio",
            "source_commit_scope": "binary source is not attested",
            "artifacts": {
                str(ROOT / "startup/hooks.py"): "a" * 64,
                str(ROOT / "startup/timing.json"): "b" * 64,
                "/local/.data/cohort/pvisor": "c" * 64,
                "/local/.data/cohort/fixtures/a/trajectory.json": "d" * 64,
                "/local/.data/cohort/fixtures/b/trajectory.json": "e" * 64,
            },
            "kernel": {
                "receipt_sha256": "f" * 64,
                "paths": {"kernel": "/local/kernel"},
                "record": {
                    "variant": "fc-system",
                    "provenance": {
                        "origin": "official-distro-stock",
                        "package_version": "test-version",
                        "pvisor_firmware": False,
                        "rpm_info": "full local RPM output",
                        "rpm_file_digest_checks": {},
                    },
                    "artifacts": {"kernel": {"file": "kernel", "sha256": "1" * 64}},
                },
            },
        }
        original = json.dumps(config)
        public = compact_configuration(config)
        self.assertEqual(json.dumps(config), original)
        self.assertEqual(public["cpu_affinity"], config["cpu_affinity"])
        self.assertEqual(public["source_commit_scope"], config["source_commit_scope"])
        self.assertEqual(
            public["artifacts"],
            {
                "pvisor": "c" * 64,
                "fixtures/a/trajectory.json": "d" * 64,
                "fixtures/b/trajectory.json": "e" * 64,
            },
        )
        self.assertEqual(public["kernel"]["receipt_sha256"], "f" * 64)
        self.assertEqual(public["kernel"]["artifacts"], {"kernel": "1" * 64})
        self.assertFalse(public["kernel"]["provenance"]["pvisor_firmware"])
        self.assertNotIn("/local", json.dumps(public))
        self.assertNotIn("rpm_info", json.dumps(public))
        self.assertEqual(compact_configuration(public), public)

    def test_output_requires_unique_correct_ordered_markers_and_no_panic(self):
        validate_output(OUTPUT, "")
        validate_output(OUTPUT + "REFERENCE_EXIT 0\n", "", guest=True)
        for stdout, stderr, guest in [
            (OUTPUT + OUTPUT, "", False),
            (OUTPUT, "Kernel panic - not syncing", False),
            (OUTPUT.replace("passed", "failed"), "", False),
            (OUTPUT, "", True),
            (OUTPUT + "REFERENCE_EXIT 1\n", "", True),
            ("REFERENCE_EXIT 0\n" + OUTPUT, "", True),
        ]:
            with self.subTest(stdout=stdout, stderr=stderr), self.assertRaises(ValueError):
                validate_output(stdout, stderr, guest)

    def test_publisher_rejects_retired_measurement_protocols_before_writing(self):
        for scope in (STARTUP.COMMON.METRIC_SCOPE, STARTUP.COMMON.READY_SCOPE):
            with self.subTest(scope=scope), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                batch = root / "batch"
                batch.mkdir()
                (batch / "manifest.json").write_text(json.dumps({"metric_scope": scope}))
                with self.assertRaisesRegex(ValueError, "paired Ready / Exit"):
                    publish_startup(batch, root / "docs")
                self.assertFalse((root / "docs").exists())

    def test_non_native_preparation_requires_explicit_current_inputs(self):
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.dict(os.environ, {}, clear=True),
            patch.object(os, "sched_getaffinity", return_value={0}, create=True),
            patch.object(os, "sched_setaffinity", create=True),
        ):
            destination = Path(temporary) / ".data/batch"
            with self.assertRaisesRegex(ValueError, "prepared inputs required"):
                STARTUP.prepare_template(
                    ROOT / "startup/pvisor_vm_timing.json",
                    destination,
                    cpu_affinity="0",
                )
            self.assertFalse((destination / "protocol.json").exists())

    def test_eight_subjects_exclude_python_taskset_and_timeout_wrappers(self):
        from subjects import SUBJECTS

        subjects = SUBJECTS
        self.assertEqual(set(subjects), set(STARTUP.BACKENDS))
        for subject in subjects.values():
            self.assertNotIn(subject[0], ("python3", "taskset", "timeout"))
        self.assertIn("128MiB", subjects["pvisor-vm"])
        self.assertIn("@ASSETS@/rootfs", subjects["pvisor-vm"])
        self.assertIn("--pull", subjects["docker"])
        self.assertIn("/bench/affinity", subjects["docker"])
        self.assertIn("@BATCH@/current/firecracker.json", subjects["fc-system"])
        for name in ("qemu", "qemu-microvm"):
            self.assertIn("@VMLINUX@", subjects[name])
            self.assertIn("@INITRD@", subjects[name])
            self.assertIn("128", subjects[name])
        self.assertIn(
            "microvm,acpi=off,x-option-roms=off,pit=off,pic=off,rtc=on", subjects["qemu-microvm"]
        )

    def test_archive_discards_only_validated_scratch_disks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for validated in (False, True):
                current = root / "current"
                current.mkdir()
                (current / "rootfs.ext4").write_bytes(b"scratch disk")
                (current / "condition.json").write_text(json.dumps({"validated": validated}))
                archive(root)
                trial = root / "trials" / ("00001" if validated else "00000")
                self.assertEqual((trial / "rootfs.ext4").exists(), not validated)
                self.assertTrue((trial / "condition.json").is_file())

    def test_listener_mismatch_is_refused_before_launch(self):
        with (
            patch.object(STARTUP.Path, "exists", return_value=True),
            patch.object(
                STARTUP.Path,
                "read_text",
                return_value=json.dumps(dict(pid=123, capability="never-print-this")),
            ),
            patch.object(STARTUP.os, "kill", return_value=None),
            patch.object(STARTUP.COMMON, "digest", side_effect=["listener", "different"]),
        ):
            with self.assertRaisesRegex(ValueError, "differs from existing Host"):
                STARTUP.preserve_listener(Path("/candidate"))

    def test_each_trial_has_fresh_staging_and_checks_host_content(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "protocol.json").write_text(json.dumps(dict(assets=None, backends=["native"])))
            prepare(root, "native")
            (root / "current/stdout").write_text(OUTPUT)
            (root / "current/stderr").write_text("")
            validate(root, "native")
            archive(root)
            prepare(root, "native")
            self.assertFalse((root / "current/stage").exists())
            (root / "current/stdout").write_text(OUTPUT)
            (root / "current/stderr").write_text("")
            (root / "current/workspace/startup-canary").write_text("changed")
            with self.assertRaisesRegex(ValueError, "workspace was modified"):
                validate(root, "native")
            self.assertEqual(len(list((root / "trials").iterdir())), 1)


@unittest.skipUnless(SEMSPEC.is_file(), "built semspec required")
class StartupIntegration(unittest.TestCase):
    def test_native_event_row_publishes_paired_metrics_for_eight_backends(self):
        with tempfile.TemporaryDirectory(prefix="paired startup ") as temporary:
            root = Path(temporary)
            affinity = str(min(os.sched_getaffinity(0)))
            batch = STARTUP.prepare_template(
                ROOT / "startup/native_timing.json", root / ".data/batch", cpu_affinity=affinity
            )
            result = subprocess.run(
                [str(SEMSPEC), "bench", "--cwd", str(batch / "workspace"), str(batch / "in.json")],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            source = root / "docs/src"
            publish_startup(batch, source)
            ready = json.loads(
                (source / "en/benchmarks/startup-timing.ready.render.json").read_text()
            )
            exited = json.loads(
                (source / "en/benchmarks/startup-timing.exit.render.json").read_text()
            )
            self.assertEqual(len(ready), 8)
            self.assertEqual(ready["Native"]["N"], 60)
            self.assertLessEqual(
                float(ready["Native"]["Ready median ms"]), float(exited["Native"]["Exit median ms"])
            )
            self.assertTrue(all(v is None for v in ready["Firecracker"].values()))
            self.assertEqual(ready["Native"]["N"], exited["Native"]["N"])
            self.assertEqual(len(list((batch / "trials").iterdir())), 64)
            csv_path = public_csv_path(root, "startup-timing.csv")
            before = csv_path.read_bytes()
            publish_startup(batch, source)
            self.assertEqual(before, csv_path.read_bytes())
            provenance_path = (
                root / "docs/overrides/assets/benchmarks/startup-timing.provenance.json"
            )
            provenance = json.loads(provenance_path.read_text())
            manifest = json.loads((batch / "manifest.json").read_text())
            self.assertEqual(
                provenance["Native"]["evidence_sha256"], manifest["exports"]["in.bench.json"]
            )
            self.assertNotIn(str(root), provenance_path.read_text())
            # Repeating a native row must preserve another cohort's configuration.
            rows = list(csv.DictReader(io.StringIO(csv_path.read_text())))
            rows.append(rows[0] | {"condition": "pvisor-host", "batch": "previous-host"})
            buffer = io.StringIO(newline="")
            writer = csv.DictWriter(buffer, fieldnames=list(rows[0]))
            writer.writeheader()
            writer.writerows(rows)
            csv_path.write_text(buffer.getvalue())
            host = dict(
                batch="previous-host",
                metric_scope=manifest["metric_scope"],
                measurement_engine="python-perf_counter_ns",
                evidence_sha256="a" * 64,
                manifest_sha256="b" * 64,
                configuration={"cpu_affinity": [3], "artifacts": {"pvisor": "c" * 64}},
            )
            # Match the retained cohort's identities to its existing CSV row.
            host["evidence_sha256"] = rows[-1]["evidence_sha256"]
            host["manifest_sha256"] = rows[-1]["manifest_sha256"]
            provenance["pVisor host"] = host
            provenance_path.write_text(json.dumps(provenance))
            raw = (batch / "manifest.json").read_bytes()
            publish_startup(batch, source)
            self.assertEqual(json.loads(provenance_path.read_text())["pVisor host"], host)
            self.assertEqual((batch / "manifest.json").read_bytes(), raw)
            for locale in ("zh", "en"):
                base = source / locale / "benchmarks"
                self.assertFalse((base / "startup-timing.provenance.json").exists())
                self.assertFalse((base / "startup-timing.render.json").exists())

    def test_native_checks_all_trials_and_publishes_json_without_editing_source_md(self):
        with tempfile.TemporaryDirectory(prefix="startup semspec ") as temporary:
            root = Path(temporary)
            affinity = {min(os.sched_getaffinity(0))}
            batch = STARTUP.prepare_template(
                ROOT / "startup/native_timing.json",
                root / ".data" / "startup",
                cpu_affinity=str(next(iter(affinity))),
            )
            original = os.sched_getaffinity(0)
            try:
                os.sched_setaffinity(0, affinity)
                result = subprocess.run(
                    [
                        str(SEMSPEC),
                        "bench",
                        "--cwd",
                        str(batch / "workspace"),
                        str(batch / "in.json"),
                    ],
                    capture_output=True,
                    text=True,
                )
            finally:
                os.sched_setaffinity(0, original)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(list((batch / "trials").iterdir())), 64)
            evidence = json.loads((batch / "in.bench.json").read_text())
            self.assertEqual(len(evidence["commands"]), 1)
            source = root / "docs/src"
            for locale in ("zh", "en"):
                page = source / locale / "benchmarks/startup.md"
                page.parent.mkdir(parents=True)
                page.write_text(
                    "# Startup\n\nhistorical Ready 100.91\n\n"
                    '{{ table("startup-timing.ready.render.json") }}\n\n'
                    '{{ table("startup-timing.exit.render.json") }}\n'
                )
            original_md = {
                locale: (source / locale / "benchmarks/startup.md").read_bytes()
                for locale in ("zh", "en")
            }
            publish_startup(batch, source)
            csv_path = public_csv_path(root, "startup-timing.csv")
            published = csv_path.read_bytes()
            for locale in ("zh", "en"):
                self.assertFalse((source / locale / "benchmarks/startup-timing.csv").exists())
            self.assertEqual(len(list(csv.DictReader(io.StringIO(csv_path.read_text())))), 1)
            for locale in ("zh", "en"):
                page = source / locale / "benchmarks/startup.md"
                self.assertEqual(page.read_bytes(), original_md[locale])
                render = subprocess.run(
                    [str(SEMSPEC), "render", str(page)], capture_output=True, text=True
                )
                self.assertEqual(render.returncode, 0, render.stderr)
                self.assertIn("| Native | 60 |", render.stdout)
                expected = evidence["commands"][0]["raw"]["Ready median ms"]
                self.assertIn(f"{expected:.2f}", render.stdout)
            (next((batch / "trials").iterdir()) / "condition.json").unlink()
            with self.assertRaisesRegex(ValueError, "missing preflight"):
                publish_startup(batch, source)
            self.assertEqual(published, csv_path.read_bytes())


if __name__ == "__main__":
    unittest.main()
