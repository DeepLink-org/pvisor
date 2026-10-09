"""Conventional harness tests, not semantic-preservation approvals."""

import json
import os
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

import immutable_lower_cache as bench


class ImmutableLowerCacheTests(unittest.TestCase):
    def test_fixture_shape(self):
        paths = list(bench.fixture_paths())
        assert len(paths) == len(set(paths)) == 2048
        assert len({p.parts[0] for p in paths}) == 32
        assert sum(len(p.parts) > 2 for p in paths) == 1024
        assert max(len(p.parts) for p in paths) == 9

    def test_seeded_complete_rounds(self):
        plan = bench.order(4207, 33)
        assert plan == bench.order(4207, 33)
        assert plan != bench.order(4208, 33)
        wanted = {(c, w) for c in bench.CONDITIONS for w in bench.WORKLOADS}
        assert len(plan) == 33
        assert all(len(cells) == len(wanted) and set(cells) == wanted for cells in plan)

    def test_paired_interval(self):
        result = bench.paired_ci([100] * 30, [50] * 30)
        assert result["percent_change"] == -50
        assert result["ci95_percent"] == [-50, -50]

    def test_profiles_keep_all_instances_and_replace_cumulative(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()

            def rec(instance, units, final):
                return dict(
                    pid=123,
                    component="overlay-core",
                    instance=instance,
                    final_record=final,
                    inclusive_spans=True,
                    measurements={"immutable_lower_cache_hits": {"units": units}},
                )

            records = [rec(1, 2, False), rec(2, 3, True), rec(1, 7, True)]
            path = tmp_path / "stderr.log"
            path.write_text("\n".join("pvisor-fs-profile " + json.dumps(r) for r in records))
            result = bench.profile_records(path)
            assert result["records"] == records
            assert len(result["instances"]) == 2
            assert (
                result["instances"][0]["measurements"]["immutable_lower_cache_hits"]["units"] == 7
            )
            assert result["missing_final"] == []

    def test_missing_final_is_explicit(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            path = tmp_path / "stderr.log"
            path.write_text(
                "pvisor-fs-profile "
                + json.dumps(
                    dict(
                        pid=1,
                        component="host-fuse",
                        instance=4,
                        final_record=False,
                        measurements={},
                    )
                )
            )
            assert bench.profile_records(path)["missing_final"] == [[1, "host-fuse", 4]]

    def test_profile_coverage_rejects_missing_components(self):
        for components in [[], ["overlay-core"], ["host-fuse"]]:
            with self.subTest(components=components):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    stage = tmp_path / "immutable-cache-on"
                    stage.mkdir()
                    records = [
                        dict(pid=1, component=c, instance=i, final_record=True, measurements={})
                        for i, c in enumerate(components)
                    ]
                    (stage / "stderr.log").write_text(
                        "\n".join("pvisor-fs-profile " + json.dumps(record) for record in records)
                    )
                    with self.assertRaisesRegex(ValueError, "incomplete profile coverage"):
                        bench.collect_profiles(tmp_path, [(stage, "immutable-cache-on")])

    def test_profile_coverage_requires_expected_log(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            with self.assertRaises(FileNotFoundError):
                bench.collect_profiles(tmp_path, [(tmp_path / "mutable", "mutable")])

    def test_profile_coverage_accepts_complete_overlay_and_empty_native(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            overlay = tmp_path / "mutable"
            native = tmp_path / "native"
            overlay.mkdir()
            native.mkdir()
            (native / "stderr.log").write_text("")
            (overlay / "stderr.log").write_text(
                "\n".join(
                    "pvisor-fs-profile "
                    + json.dumps(
                        dict(pid=1, component=c, instance=i, final_record=True, measurements={})
                    )
                    for i, c in enumerate(["overlay-core", "host-fuse"])
                )
            )
            profiles = bench.collect_profiles(tmp_path, [(overlay, "mutable"), (native, "native")])
            assert len(profiles["mutable"]["instances"]) == 2
            assert profiles["native"]["instances"] == []

    @unittest.skipUnless(hasattr(os, "listxattr"), "requires Linux extended attribute APIs")
    def test_inventory_detects_content_namespace_metadata_changes(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            (tmp_path / "a").write_text("abc")
            first = bench.inventory(tmp_path)
            assert first["a"]["sha256"] == bench.sha(tmp_path / "a")
            (tmp_path / "a").write_text("def")
            assert first != bench.inventory(tmp_path)
            (tmp_path / "b").write_text("new")
            assert set(bench.inventory(tmp_path)) == {".", "a", "b"}
