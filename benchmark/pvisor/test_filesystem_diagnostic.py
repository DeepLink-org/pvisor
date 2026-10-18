import json
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from filesystem_diagnostic import collect_profiles, write_counter_csv


def record(pid=1, calls=2, final=False, total_ns=2000):
    return "pvisor-fs-profile " + json.dumps(
        dict(
            schema=2,
            pid=pid,
            component="core",
            instance=1,
            final_record=final,
            measurements={"read": dict(calls=calls, total_ns=total_ns, units=100)},
        )
    )


class FilesystemDiagnosticTests(unittest.TestCase):
    def test_complete_profiles_require_every_registered_instance(self):
        def start(instance=1, component="core"):
            return "pvisor-fs-profile-start " + json.dumps(
                dict(schema=1, pid=1, component=component, instance=instance)
            )

        complete = [start(), record(final=True)]
        assert (
            len(collect_profiles(complete, require_complete=True, expected_components={"core"}))
            == 1
        )
        with self.assertRaisesRegex(ValueError, "incomplete profile instance coverage"):
            collect_profiles(complete + [start(instance=2)], require_complete=True)
        with self.assertRaisesRegex(ValueError, "expected profile components"):
            collect_profiles(
                complete, require_complete=True, expected_components={"core", "protocol"}
            )
        with self.assertRaisesRegex(ValueError, "registrations"):
            collect_profiles([record(final=True)], require_complete=True)
        with self.assertRaisesRegex(ValueError, "final profile"):
            collect_profiles([start(), record()], require_complete=True)

    def test_profiles_keep_process_identity_and_do_not_add_cumulative_records(self):
        profiles = collect_profiles(
            [record(), record(calls=5, total_ns=5000, final=True), record(pid=2)]
        )
        assert len(profiles) == 2
        assert profiles[0]["measurements"]["read"]["calls"] == 5
        assert profiles[0]["final_record"]
        assert profiles[1]["pid"] == 2

    def test_corrupt_or_unidentifiable_counters_cannot_prove_completed_work(self):
        with self.assertRaisesRegex(ValueError, "nonmonotonic"):
            collect_profiles([record(calls=5), record(calls=2)])
        value = json.loads(record().removeprefix("pvisor-fs-profile "))
        value.pop("pid")
        with self.assertRaisesRegex(ValueError, "process identity"):
            collect_profiles(["pvisor-fs-profile " + json.dumps(value)])

    def test_partial_counters_and_missing_maximum_are_not_reported_as_complete_or_zero(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            profiles = collect_profiles([record()])
            path = tmp_path / "counters.csv"
            write_counter_csv(
                [dict(backend="pvisor-vm", mode="filesystem", trial=0, filesystem=profiles)], path
            )
            import csv

            with path.open() as stream:
                row = next(csv.DictReader(stream))
            assert row["coverage"] == "partial-lower-bound"
            assert row["max_inclusive_ms"] == ""
            assert row["calls"] == "2"
