import copy
import json
import tempfile
import unittest
from pathlib import Path

from phase_profiles import (
    COMPONENTS,
    PHASE_PREFIX,
    SCOPES,
    collect_phase_profiles,
    instrument_rootfs,
)


def fixture():
    lines, snapshots = [], {}
    number = 0
    for scope in SCOPES:
        snapshots[scope] = {}
        for component in sorted(COMPONENTS):
            number += 1
            profile = dict(
                schema=2,
                pid=111,
                component=component,
                instance=number,
                inclusive_spans=True,
                final_record=False,
                measurements={},
            )
            lines.append(
                "pvisor-fs-profile-start "
                + json.dumps({key: profile[key] for key in ("pid", "component", "instance")})
            )
            snapshots[scope][component] = profile
    for token in (1, 2):
        for scope in SCOPES:
            profiles = copy.deepcopy(snapshots[scope])
            for component, profile in profiles.items():
                measurements = profile["measurements"]
                measurements[
                    "LOOKUP" if component == "virtio-fs-protocol" else "layer_leaf_stats"
                ] = dict(calls=token * 5, units=0, total_ns=token * 99)
                if component == "virtio-fs-protocol":
                    measurements["IOCTL"] = dict(calls=token, units=0, total_ns=token * 10)
                    measurements["request_bytes"] = dict(calls=0, units=token * 180, total_ns=0)
                if component == "virtio-fs-dispatch":
                    measurements["direct_requests"] = dict(calls=0, units=token * 6, total_ns=0)
            lines.append(
                PHASE_PREFIX
                + json.dumps(
                    dict(schema=1, scope=scope, token=token, profiles=list(profiles.values()))
                )
            )
            if token == 2:
                for profile in profiles.values():
                    profile["final_record"] = True
                    lines.append("pvisor-fs-profile " + json.dumps(profile))
    return lines


class PhaseProfileTests(unittest.TestCase):
    def test_exact_scope_interval_keeps_raw_and_corrects_marker_counts(self):
        report = collect_phase_profiles(fixture(), operations=("rg",), rounds=1)
        self.assertEqual(len(report["rows"]), 8)
        self.assertEqual(report["unscoped_final_profiles"], [])
        row = next(
            row
            for row in report["rows"]
            if row["scope"] == "rootfs" and row["component"] == "virtio-fs-protocol"
        )
        self.assertEqual(row["raw_measurements"]["IOCTL"]["calls"], 1)
        self.assertEqual(row["measurements"]["IOCTL"]["calls"], 0)
        self.assertEqual(row["measurements"]["LOOKUP"]["calls"], 5)
        self.assertEqual(row["measurements"]["request_bytes"]["units"], 100)
        self.assertNotIn("total_ns", row["measurements"]["LOOKUP"])
        dispatch = next(
            row
            for row in report["rows"]
            if row["scope"] == "rootfs" and row["component"] == "virtio-fs-dispatch"
        )
        self.assertEqual(dispatch["raw_measurements"]["direct_requests"]["units"], 6)
        self.assertEqual(dispatch["measurements"]["direct_requests"]["units"], 5)

    def test_missing_whole_instance_is_unknown_work(self):
        lines = fixture()
        lines.append(
            "pvisor-fs-profile-start "
            + json.dumps(dict(schema=1, pid=111, component="overlay-core", instance=999))
        )
        with self.assertRaisesRegex(ValueError, "incomplete profile instance coverage"):
            collect_phase_profiles(lines, operations=("rg",), rounds=1)

    def test_missing_scope_or_component_rejects_partial_evidence(self):
        lines = fixture()
        scope_missing = [
            line
            for line in lines
            if not (
                line.startswith(PHASE_PREFIX)
                and json.loads(line.removeprefix(PHASE_PREFIX))["scope"] == SCOPES[1]
            )
        ]
        with self.assertRaisesRegex(ValueError, "phase token coverage"):
            collect_phase_profiles(scope_missing, operations=("rg",), rounds=1)
        for index, line in enumerate(lines):
            if line.startswith(PHASE_PREFIX):
                phase = json.loads(line.removeprefix(PHASE_PREFIX))
                phase["profiles"].pop()
                lines[index] = PHASE_PREFIX + json.dumps(phase)
                break
        with self.assertRaisesRegex(ValueError, "mounted component coverage"):
            collect_phase_profiles(lines, operations=("rg",), rounds=1)

    def test_duplicate_token_and_counter_regression_are_rejected(self):
        lines = fixture()
        duplicate = next(line for line in lines if line.startswith(PHASE_PREFIX))
        with self.assertRaisesRegex(ValueError, "duplicate phase token"):
            collect_phase_profiles(lines + [duplicate], operations=("rg",), rounds=1)
        for index, line in enumerate(lines):
            if line.startswith(PHASE_PREFIX):
                phase = json.loads(line.removeprefix(PHASE_PREFIX))
                if phase["token"] == 2:
                    phase["profiles"][0]["measurements"]["layer_leaf_stats"]["calls"] = 1
                    lines[index] = PHASE_PREFIX + json.dumps(phase)
                    break
        with self.assertRaisesRegex(ValueError, "nonmonotonic"):
            collect_phase_profiles(lines, operations=("rg",), rounds=1)

    def test_unscoped_lifecycle_owner_retained_but_not_attributed_to_tools(self):
        lines = fixture()
        profile = dict(
            schema=2,
            pid=111,
            component="overlay-core",
            instance=999,
            inclusive_spans=True,
            final_record=True,
            measurements={},
        )
        lines += [
            "pvisor-fs-profile-start "
            + json.dumps({key: profile[key] for key in ("pid", "component", "instance")}),
            "pvisor-fs-profile " + json.dumps(profile),
        ]
        report = collect_phase_profiles(lines, operations=("rg",), rounds=1)
        self.assertEqual(len(report["unscoped_final_profiles"]), 1)

    def test_instrumentation_preserves_checked_command_and_refuses_second_patch(self):
        source = Path(__file__).with_name("filesystem_rounds_workload.py").read_text()
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "bench/filesystem_rounds_workload.py"
            target.parent.mkdir()
            target.write_text(source)
            instrument_rootfs(directory)
            changed = target.read_text()
            self.assertIn(
                'output = checked([str(root / "usr/bin/python3"), str(script), operation],', changed
            )
            self.assertEqual(changed.count("checkpoint(phase_descriptors, phase_token)"), 2)
            self.assertTrue(target.with_name("phase_profiles.py").is_file())
            with self.assertRaisesRegex(ValueError, "unexpected resident workload"):
                instrument_rootfs(directory)


if __name__ == "__main__":
    unittest.main()
