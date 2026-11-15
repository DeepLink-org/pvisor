"""Read-only contracts for retained Ready/Exit cohorts; Rust observes new runs."""

import importlib.util
import json
import shlex
import sys
import unittest
from copy import deepcopy
from pathlib import Path

from tests.fixtures import workspace

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "startup"))
SPEC = importlib.util.spec_from_file_location("ready_archive_audit", ROOT / "startup/audit.py")
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


def frozen_ready_batch(root, backend="pvisor-vm"):
    """Construct historical evidence; never execute the recorded subject/argv."""
    root.mkdir(parents=True, exist_ok=True)
    subject = ["/frozen/pvisor", "spawn", "--vm"]
    protocol = dict(
        subject=subject,
        scope=AUDIT.COMMON.READY_EXIT_SCOPE,
        measurement_engine="python-perf_counter_ns",
        ready="exact REFERENCE_READY line received",
        guest_cpus=2,
        guest_memory_mib=128,
        backends=[backend],
        assets=None,
        input_before=None,
        kernel=None,
        cache="host cache retained",
    )
    AUDIT.COMMON.save(root / "protocol.json", protocol)
    samples = []
    phases = (
        [("preflight", 0)] + [("warmup", i) for i in range(3)] + [("sample", i) for i in range(60)]
    )
    for i, (phase, index) in enumerate(phases):
        trial = root / f"trials/{i:05d}"
        # Large non-formal durations prove preflight/warmup exclusion.
        duration = (index + 1) * 1_000_000 if phase == "sample" else 500_000_000
        start = (i + 1) * 1_000_000_000
        AUDIT.COMMON.save(
            trial / "timing.json",
            dict(
                command=subject,
                phase=phase,
                index=index,
                start_ns=start,
                ready_ns=start + duration,
                end_ns=start + duration + 1_000_000,
                ready_duration_ns=duration,
                exit_duration_ns=duration + 1_000_000,
                exit_code=0,
            ),
        )
        (trial / "stdout").write_text(
            'REFERENCE_READY\nREFERENCE_RESULT {"mode":"ready","correctness":"passed"}\n'
        )
        (trial / "stderr").touch()
        AUDIT.COMMON.save(trial / "condition.json", dict(backend=backend, validated=True))
        AUDIT.COMMON.save(
            trial / "stage/run-bundle.json",
            dict(
                run=dict(
                    state="completed", exit_code=0, executor=dict(isolation="virtual_machine")
                ),
                safety=dict(filesystem_changes_staged=True),
            ),
        )
        if phase == "sample":
            samples.append(dict(ready_ns=duration, exit_ns=duration + 1_000_000))
    result = AUDIT.summarize_pairs(samples)
    AUDIT.COMMON.save(
        root / "timing.samples.json",
        dict(engine="python-perf_counter_ns", unit="nanosecond", samples=samples, result=result),
    )
    label = AUDIT.LABELS[backend]
    command = shlex.join(
        ["python3", str(ROOT / "startup" / (backend.replace("-", "_") + "_timing.py")), str(root)]
    )
    template, resolved = {label: "$ " + command}, {label: result}
    evidence = dict(
        format=1,
        cwd=str(root),
        template=str(root / "in.json"),
        commands=[dict(kind="script-row", command=command, raw=result, value=result)],
        cells=[],
        rows=[dict(row=label, command=command, expression=command, columns=list(AUDIT.FIELDS))],
    )
    AUDIT.COMMON.save(root / "in.json", template)
    AUDIT.COMMON.save(root / "in.bench.json", evidence)
    AUDIT.COMMON.save(root / "in.render.json", resolved)
    # Synthetic frozen source, not a current measurement harness.
    AUDIT.COMMON.save(root / "harness/source.json", {"historical": True})
    manifest = dict(
        status="prepared",
        batch=root.name,
        cwd=str(root),
        role="user-facing",
        metric_scope=AUDIT.COMMON.READY_EXIT_SCOPE,
        entry="startup/" + backend.replace("-", "_") + "_timing.json",
        conditions=[backend],
        benchmark_id="B-STARTUP",
        measurement_engine="python-perf_counter_ns",
        python_version="historical-test",
        runs=60,
        warmup=3,
        rows={label: backend},
        cpu_affinity=[0, 1],
        source_commit_scope="binary source is not attested",
        artifacts={str(root / "protocol.json"): AUDIT.COMMON.digest(root / "protocol.json")},
        harness={"source.json": AUDIT.COMMON.digest(root / "harness/source.json")},
        input_sha256=AUDIT.COMMON.digest(root / "in.json"),
    )
    AUDIT.COMMON.save(root / "manifest.json", manifest)
    return manifest, evidence, resolved, template


class ReadyArchiveContracts(unittest.TestCase):
    def test_statistics_use_nanoseconds_and_sample_sd(self):
        result = AUDIT.ARCHIVE.summarize_ready([1_000_000, 3_000_000, 5_000_000])
        self.assertEqual(result, dict(zip(AUDIT.ARCHIVE.READY_FIELDS, [3, 3, 3, 2, 1, 5])))
        for invalid in ([1], [True, 2], [0, 2], [1.1, 2]):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                AUDIT.ARCHIVE.summarize_ready(invalid)

    def test_paired_summary_rejects_unpaired_and_reversed_durations(self):
        valid = [
            dict(ready_ns=1_000_000, exit_ns=3_000_000),
            dict(ready_ns=3_000_000, exit_ns=5_000_000),
        ]
        result = AUDIT.summarize_pairs(valid)
        self.assertEqual(
            (result["N"], result["Ready median ms"], result["Exit median ms"]), (2, 2, 4)
        )
        for invalid in (
            [{"ready_ns": 1}],
            [dict(ready_ns=2, exit_ns=1)],
            [dict(ready_ns=True, exit_ns=3)],
            [valid[0]],
        ):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                AUDIT.summarize_pairs(invalid)

    def test_complete_archive_recomputes_formal_samples_without_running_subjects(self):
        with workspace() as (root, resources):
            args = frozen_ready_batch(root)
            before = {p: p.read_bytes() for p in root.rglob("*") if p.is_file()}
            self.assertEqual(set(AUDIT.audit_timing(root, *args)), {"pvisor-vm"})
            self.assertEqual(args[2]["pVisor VM"]["Ready median ms"], 30.5)
            self.assertEqual(args[2]["pVisor VM"]["Exit median ms"], 31.5)
            self.assertEqual({p: p.read_bytes() for p in before}, before)
            self.assertEqual(json.loads((root / "audit.json").read_text())["status"], "passed")

    def test_trial_tampering_is_rejected_including_nonformal_trials(self):
        mutations = [
            ("command", ["/different/subject"]),
            ("phase", "wrong"),
            ("index", 100),
            ("exit_code", 7),
            ("start_ns", True),
            ("ready_ns", 0),
            ("end_ns", 0),
            ("ready_duration_ns", 1),
            ("exit_duration_ns", 1),
        ]
        with workspace() as (root, resources):
            args = frozen_ready_batch(root)
            for trial in ("00000", "00001", "00004"):
                path = root / "trials" / trial / "timing.json"
                original = path.read_bytes()
                for key, value in mutations:
                    with self.subTest(trial=trial, key=key):
                        record = json.loads(original)
                        record[key] = value
                        AUDIT.COMMON.save(path, record)
                        with self.assertRaises(ValueError):
                            AUDIT.audit_timing(root, *args)
                path.write_bytes(original)

    def test_output_bundle_inventory_and_summary_tampering_is_rejected(self):
        with workspace() as (root, resources):
            args = frozen_ready_batch(root)
            changes = [
                ("trials/00000/stdout", b"REFERENCE_READY"),
                ("trials/00001/stdout", b"REFERENCE_READY\nREFERENCE_READY\n"),
                ("trials/00004/stderr", b"Kernel panic - not syncing"),
                ("trials/00004/condition.json", b'{"backend":"pvisor-vm","validated":false}'),
                (
                    "trials/00004/stage/run-bundle.json",
                    b'{"run":{"state":"failed","exit_code":0,"executor":{"isolation":"virtual_machine"}},'
                    b'"safety":{"filesystem_changes_staged":true}}',
                ),
                ("timing.samples.json", b"{}"),
            ]
            for name, altered in changes:
                path = root / name
                original = path.read_bytes()
                with self.subTest(name=name):
                    path.write_bytes(altered)
                    with self.assertRaises(ValueError):
                        AUDIT.audit_timing(root, *args)
                path.write_bytes(original)
            (root / "trials/00001/timing.json").unlink()
            with self.assertRaisesRegex(ValueError, "missing Ready"):
                AUDIT.audit_timing(root, *args)

    def test_protocol_argv_and_resolved_rows_are_not_interchangeable(self):
        with workspace() as (root, resources):
            args = frozen_ready_batch(root)
            for position, key, value in [
                (0, "measurement_engine", "criterion-rust"),
                (0, "runs", 59),
                (0, "conditions", ["unknown"]),
                (3, "pVisor VM", args[3]["pVisor VM"] + " --extra"),
                (2, "pVisor VM", args[2]["pVisor VM"] | {"Ready mean ms": 0}),
                (1, "rows", []),
            ]:
                altered = deepcopy(args)
                altered[position][key] = value
                with self.subTest(position=position, key=key), self.assertRaises(ValueError):
                    AUDIT.audit_timing(root, *altered)
            protocol = root / "protocol.json"
            protocol.write_text(protocol.read_text() + "\n")
            with self.assertRaisesRegex(ValueError, "checksum"):
                AUDIT.audit_timing(root, *args)

    def test_frozen_source_and_input_digests_are_checked_before_accepting_archive(self):
        from update import validate_batch

        with workspace() as (root, resources):
            frozen_ready_batch(root)
            for name, message in (
                ("harness/source.json", "frozen harness changed"),
                ("in.json", "benchmark input changed"),
            ):
                path = root / name
                original = path.read_bytes()
                with self.subTest(name=name):
                    path.write_bytes(original + b"\n")
                    with self.assertRaisesRegex(ValueError, message):
                        validate_batch(root)
                path.write_bytes(original)


if __name__ == "__main__":
    unittest.main()
