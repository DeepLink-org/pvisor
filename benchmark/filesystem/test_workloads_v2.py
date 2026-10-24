"""Offline protocol contracts; no compilation, KVM, service or benchmark launch."""

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import workloads_v2 as V2  # noqa: E402 - import after module-loading setup


class WorkloadPlanContracts(unittest.TestCase):
    def test_catalog_and_fresh_sampling_do_not_turn_rounds_into_trials(self):
        plan = V2.make_plan()
        self.assertEqual(
            [c["id"] for c in plan["catalog"]],
            [f"M{x:02}" for x in range(1, 13)]
            + [f"R{x:02}" for x in range(1, 5)]
            + ["S01", "S02", "S03"],
        )
        self.assertEqual(plan["inventory"]["layers"], dict(mechanism=12, resource=4, scenario=3))
        self.assertEqual(plan["sampling"]["planned_vm_environments"], 190)
        self.assertEqual(plan["sampling"]["planned_vm_cell_round_rows"], 570)
        envs = plan["environment_schedule"]
        self.assertEqual(len({e["environment_id"] for e in envs}), 190)
        for case in plan["inventory"]["selected_cell_ids"]:
            for pair in range(5):
                selected = [e for e in envs if e["case_id"] == case and e["pair"] == pair]
                self.assertEqual({e["condition"] for e in selected}, {"baseline", "candidate"})
                self.assertTrue(
                    all(
                        e["fresh_environment"] and e["resident_rounds"] == [1, 2, 3]
                        for e in selected
                    )
                )
        self.assertFalse(plan["cache_state"]["cross_case_reuse"])

    def test_resource_workers_hold_total_work_and_file_count_constant(self):
        for memory in (256, 512, 1024):
            resources = [
                c["parameters"] for c in V2.case_catalog(memory) if c["layer"] == "resource"
            ]
            self.assertEqual({r["logical_write_bytes"] for r in resources}, {2 * memory * V2.MIB})
            self.assertEqual({r["file_count"] for r in resources}, {4})
            self.assertEqual({r["block_bytes"] for r in resources}, {4096})
            self.assertTrue(all(0 < r["buffer_bytes_per_worker"] <= V2.MIB for r in resources))
            self.assertTrue(
                all(
                    r["buffer_bytes_total"] == V2.MIB
                    and r["workers"] * r["buffer_bytes_per_worker"] <= V2.MIB
                    for r in resources
                )
            )
            for numerator in (1, 5):
                pair = [
                    r for r in resources if r["working_set_ram_ratio"]["numerator"] == numerator
                ]
                self.assertEqual({r["workers"] for r in pair}, {1, 4})
                self.assertEqual(
                    {r["working_set_bytes"] for r in pair}, {memory * V2.MIB * numerator // 4}
                )

    def test_comparisons_change_only_their_isolated_flag_and_require_ownership(self):
        for name, flag in zip(V2.COMPARISONS, (V2.GUEST_WB, V2.HOST_WB, V2.DATA)):
            plan = V2.make_plan(comparison=name)
            baseline, candidate = [
                a["requested_environment"] for a in plan["comparison_contract"]["arms"]
            ]
            self.assertEqual({k for k in baseline if baseline[k] != candidate[k]}, {flag})
            for env in (baseline, candidate):
                self.assertNotEqual((env[V2.GUEST_WB], env[V2.HOST_WB]), ("1", "1"))
                if env[V2.DATA] == "1":
                    self.assertEqual((env[V2.GUEST_WB], env[V2.HOST_WB]), ("0", "0"))
                    self.assertIn(
                        "authenticated Immutable",
                        plan["comparison_contract"]["input_ownership_requirement"],
                    )
            self.assertFalse(plan["comparison_contract"]["qualification_verified"])
        data = V2.make_plan(comparison="lower-read-cache")
        self.assertEqual(data["inventory"]["selected_cells"], 9)
        self.assertEqual(data["sampling"]["planned_vm_environments"], 90)
        self.assertNotIn("R01", data["inventory"]["selected_cell_ids"])
        self.assertNotIn("M06", data["inventory"]["selected_cell_ids"])
        self.assertEqual(
            {c["id"] for c in data["curve_families"]}, {"metadata_size", "read_working_set"}
        )
        read_axis = next(c for c in data["curve_families"] if c["id"] == "read_working_set")
        self.assertIn("logical_read_bytes_per_pass", read_axis["derived"])
        self.assertIn("not fixed", read_axis["normalization"])
        self.assertEqual(
            {c["id"] for c in data["catalog"] if c.get("primary")}, {"S01", "S02", "S03"}
        )
        self.assertTrue(
            all(
                c["id"] in data["inventory"]["selected_cell_ids"]
                for c in data["catalog"]
                if c.get("primary")
            )
        )

    def test_seed_replays_schedule_and_native_reference_cannot_change_ab(self):
        first = V2.make_plan(seed=123)
        self.assertEqual(first, V2.make_plan(seed=123))
        self.assertNotEqual(first["paired_blocks"], V2.make_plan(seed=124)["paired_blocks"])
        native = V2.make_plan(seed=123, include_native_reference=True)
        self.assertEqual(first["paired_blocks"], native["paired_blocks"])
        self.assertEqual(first["environment_schedule"], native["environment_schedule"])
        self.assertEqual(native["native_reference"]["planned_environments"], 95)
        self.assertTrue(native["native_reference"]["reference_only"])
        self.assertFalse(native["native_reference"]["causal_contrast"])
        self.assertTrue(
            set(e["environment_id"] for e in first["environment_schedule"]).isdisjoint(
                e["environment_id"] for e in native["native_reference"]["environment_schedule"]
            )
        )

    def test_layer_filter_intersects_applicability_without_changing_case_pair_orders(self):
        for comparison in V2.COMPARISONS:
            full = V2.make_plan(comparison=comparison, seed=45)
            for layer, catalog_count in [("mechanism", 12), ("resource", 4), ("scenario", 3)]:
                plan = V2.make_plan(comparison=comparison, seed=45, layer=layer)
                selected = {
                    c["id"]
                    for c in full["catalog"]
                    if c["layer"] == layer and c["comparison_applicability"][comparison]["eligible"]
                }
                self.assertEqual(len(plan["catalog"]), 19)
                self.assertEqual(sum(c["layer"] == layer for c in plan["catalog"]), catalog_count)
                self.assertEqual(set(plan["inventory"]["selected_cell_ids"]), selected)
                self.assertEqual(plan["sampling"]["planned_vm_environments"], 10 * len(selected))
                self.assertEqual(plan["sampling"]["planned_vm_cell_round_rows"], 30 * len(selected))
                self.assertTrue(all(c["anchor"] in selected for c in plan["curve_families"]))
                expected = {
                    (b["case_id"], b["pair"]): b["condition_order"]
                    for b in full["paired_blocks"]
                    if b["case_id"] in selected
                }
                actual = {
                    (b["case_id"], b["pair"]): b["condition_order"] for b in plan["paired_blocks"]
                }
                self.assertEqual(actual, expected)
                self.assertEqual(
                    {e["environment_id"] for e in plan["environment_schedule"]},
                    {
                        e["environment_id"]
                        for e in full["environment_schedule"]
                        if e["case_id"] in selected
                    },
                )
        lifecycle = next(c for c in full["catalog"] if c["id"] == "M12")
        steps = lifecycle["parameters"]["steps"]
        self.assertLess(steps.index("write"), steps.index("file_fsync"))
        self.assertLess(steps.index("file_fsync"), steps.index("checked_close"))
        self.assertEqual(steps[-3:], ["rename", "unlink", "directory_fsync"])
        self.assertIn("before unlink", lifecycle["parameters"]["oracle_order"][0])

    def test_plan_is_not_measurement_and_curves_do_not_expand_cartesian_product(self):
        for profile in ("quick", "curves"):
            plan = V2.make_plan(profile=profile)
            self.assertEqual(plan["planner"], "implemented")
            self.assertEqual(plan["executor"], "not_implemented")
            self.assertFalse(plan["execution"]["supported"])
            self.assertIsNone(plan["execution"]["command"])
            self.assertFalse(plan["performance_samples"])
            self.assertEqual(plan["measured_environments"], 0)
            self.assertEqual(plan["measured_cell_round_rows"], 0)
            self.assertIsNone(plan["results"])
            self.assertIsNone(plan["metrics"]["aggregate_score"])
            self.assertEqual(len(plan["metrics"]["co_primary"]), 3)
            self.assertEqual(len(plan["environment_schedule"]), 190)
            self.assertEqual(len(plan["curve_families"]), 7)
            self.assertTrue(
                all(
                    x["status"] == "deferred" and x["axis"] not in x["fixed"]
                    for x in plan["curve_families"]
                )
            )

    def test_cli_exclusive_output_refuses_files_and_symlinks_without_changing_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "plan.json"
            args = ["--comparison", "guest-writeback", "--output", str(output)]
            self.assertEqual(V2.main(args), 0)
            before = output.read_bytes()
            self.assertEqual(json.loads(before)["measured_environments"], 0)
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                V2.main(args)
            self.assertEqual(output.read_bytes(), before)
            link = Path(directory) / "link.json"
            link.symlink_to(output)
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                V2.main(["--comparison", "guest-writeback", "--output", str(link)])
            self.assertEqual(output.read_bytes(), before)
            self.assertTrue(link.is_symlink())
            dangling = Path(directory) / "dangling.json"
            dangling.symlink_to(Path(directory) / "missing-target.json")
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                V2.main(["--comparison", "guest-writeback", "--output", str(dangling)])
            self.assertTrue(dangling.is_symlink())
            self.assertFalse((Path(directory) / "missing-target.json").exists())

    def test_invalid_inputs_and_execution_flags_fail_before_output(self):
        for kwargs in [
            dict(guest_memory_mib=0),
            dict(guest_memory_mib=True),
            dict(seed=-1),
            dict(seed=True),
            dict(profile="execute"),
            dict(comparison="combined"),
        ]:
            with self.subTest(kwargs=kwargs), self.assertRaises(ValueError):
                V2.make_plan(**kwargs)
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "must-not-exist.json"
            for flag in ("--execute", "--measure", "--run"):
                with (
                    self.subTest(flag=flag),
                    contextlib.redirect_stderr(io.StringIO()),
                    self.assertRaises(SystemExit),
                ):
                    V2.main(["--comparison", "guest-writeback", "--output", str(output), flag])
                self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
