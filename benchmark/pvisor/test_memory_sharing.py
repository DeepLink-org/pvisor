"""Public sharing protocol checks, without VM or systemd access."""

import copy
import json
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from memory_scale import validate_report
from memory_sharing import matrix
from publish_memory_sharing import load_cohort
from test_memory_scale import valid


class MemorySharingTests(unittest.TestCase):
    def test_public_matrix_separates_baseline_and_dynamic_dedup_controls(self):
        cells = matrix()
        assert len(cells) == 36 and len(set(cells)) == 36
        for n in (1, 2, 4):
            for pattern in ("repeated", "random-shared", "random-unique"):
                assert (n, pattern, "independent") in cells
                assert (n, pattern, "shared") in cells
                assert (n, pattern, "ksm-off") in cells
                assert (n, pattern, "ksm-on") in cells

    def test_independent_control_requires_distinct_physical_ram_inodes(self):
        raw, config = valid(scanner="1")
        config["independent_inodes"] = True
        config["cpus"] = 2
        raw["conditions"]["independent_inodes"] = True
        raw["conditions"]["cpus"] = 2
        raw["profile"]["cpus"] = 2
        with self.assertRaisesRegex(ValueError, "inode proof"):
            validate_report(raw, config)
        raw["checks"] += [
            dict(
                name="independent_ram_inode",
                passed=True,
                evidence=dict(instance=i, device=1, inode=i, bytes=256 * 1024**2),
            )
            for i in (1, 2)
        ]
        validate_report(raw, config)
        bad = copy.deepcopy(raw)
        bad["checks"][-1]["evidence"]["inode"] = 1
        with self.assertRaisesRegex(ValueError, "inode proof"):
            validate_report(bad, config)

    def test_engineering_and_preflight_cohorts_cannot_publish(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            value = dict(
                schema="pvisor-memory-sharing-cohort/v1",
                role="engineering A/B",
                complete=True,
                arguments=dict(preflight=False, samples=30, warmups=3, scan_seconds=30),
                selected=matrix(),
                input_verification={"rootfs": True, "firmware": True},
            )
            path = tmp_path / "report.json"
            for changes in (
                {},
                {
                    "role": "user-facing",
                    "arguments": dict(preflight=True, samples=1, warmups=0, scan_seconds=2),
                },
            ):
                path.write_text(json.dumps(value | changes))
                with self.assertRaisesRegex(ValueError, "public sharing cohort"):
                    load_cohort(path)

    def test_ksm_sixty_second_override_keeps_other_strategy_windows_short(self):
        from types import SimpleNamespace

        from memory_sharing import scan_seconds

        args = SimpleNamespace(scan_seconds=2, ksm_scan_seconds=60)
        assert scan_seconds(args, "ksm") == 60
        for arm in ("unshared", "snapshot-cow", "daemon-pool"):
            assert scan_seconds(args, arm) == 2
        assert scan_seconds(SimpleNamespace(scan_seconds=30), "ksm-on") == 30
