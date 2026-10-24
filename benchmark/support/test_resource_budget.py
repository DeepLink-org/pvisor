"""Budget witnesses must reject missing controls and escaped process scopes."""

import tempfile
import unittest
from contextlib import ExitStack
from dataclasses import replace
from pathlib import Path
from unittest import mock

from resource_budget import (
    BudgetViolation,
    ObservationChanged,
    ResourceBudget,
    parse_cpus,
    unified_path,
)


def process_stat(pid=123, state="S", started=42):
    fields = [state] + ["0"] * 18 + [str(started)]
    return f"{pid} (worker ) name) " + " ".join(fields) + "\n"


def affinity_fixture(fixture):
    budget, group, task = fixture
    (group / "cpuset.cpus.effective").unlink()
    for tid in [123, 124]:
        thread = task / "task" / str(tid)
        thread.mkdir(parents=True)
        (thread / "stat").write_text(process_stat(pid=tid))
        (thread / "cgroup").write_text("0::/private.slice/worker.service\n")
        (thread / "status").write_text("Cpus_allowed_list:\t0-1\n")
    return replace(budget, cpu_placement="affinity"), group, task


def subtree_fixture(fixture):
    budget, group, task = fixture
    (group / "cgroup.procs").write_text("")
    worker = group / "worker.service"
    worker.mkdir()
    (worker / "cgroup.procs").write_text("123\n")
    detached = group / "detached.scope"
    detached.mkdir()
    (detached / "cgroup.procs").write_text("456\n")
    helper = task.parent / "456"
    helper.mkdir()
    (helper / "stat").write_text(process_stat(pid=456, started=99))
    (helper / "cgroup").write_text("0::/private.slice/detached.scope\n")
    return budget, group, worker, detached, helper


def _make_fixture(tmp_path):
    cgroups = tmp_path / "cgroups"
    group = cgroups / "private.slice"
    group.mkdir(parents=True)
    files = {
        "cgroup.type": "domain",
        "memory.max": "1073741824",
        "memory.swap.max": "0",
        "cpu.max": "200000 100000",
        "cpuset.cpus.effective": "0-1",
        "memory.current": "123456",
        "memory.peak": "234567",
        "memory.stat": "anon 120000\nfile 3456\n",
        "memory.events": "max 0\noom 0\noom_kill 0\n",
        "cpu.stat": "usage_usec 123\nuser_usec 100\nsystem_usec 23\n",
    }
    for name, value in files.items():
        (group / name).write_text(value.rstrip("\n") + "\n")
    proc = tmp_path / "proc"
    task = proc / "123"
    task.mkdir(parents=True)
    (task / "stat").write_text(process_stat())
    (task / "cgroup").write_text("0::/private.slice/worker.service\n")
    budget = ResourceBudget(
        group, 1073741824, frozenset({0, 1}), cgroup_root=cgroups, proc_root=proc
    )
    return budget, group, task


class ResourceBudgetTests(unittest.TestCase):
    def test_witness_covers_actual_budget_and_named_process(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, _ = fixture
            result = budget.processes([123])
            assert result["before"]["memory_max_bytes"] == 1073741824
            assert result["after"]["effective_cpus"] == [0, 1]
            assert result["witnesses"] == [
                dict(
                    pid=123,
                    start_ticks=42,
                    cgroup="/private.slice/worker.service",
                    scope="this live PID at observation; not all descendants or future membership",
                )
            ]

    def test_reject_unenforced_or_different_budget(self):
        for name, value in [
            ("memory.max", "max"),
            ("memory.max", "2147483648"),
            ("memory.swap.max", "max"),
            ("memory.swap.max", "1"),
            ("cpu.max", "max 100000"),
            ("cpu.max", "100000 100000"),
            ("cpu.max", "200000 0"),
            ("cpuset.cpus.effective", "0-15"),
            ("cpuset.cpus.effective", "0"),
            ("cgroup.type", "threaded"),
        ]:
            with self.subTest(name=name, value=value):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    fixture = _make_fixture(tmp_path)
                    budget, group, _ = fixture
                    (group / name).write_text(value)
                    with self.assertRaises(ValueError):
                        budget.processes([123])

    def test_reject_escaped_or_ambiguous_membership(self):
        for membership in [
            "0::/private.slice-other/worker.service\n",
            "0::/outside.service\n",
            "0::/private.slice/../outside.service\n",
            "0::/\n",
            "1:memory:/private.slice\n",
            "0::/private.slice\n0::/private.slice\n",
        ]:
            with self.subTest(membership=membership):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    fixture = _make_fixture(tmp_path)
                    budget, _, task = fixture
                    (task / "cgroup").write_text(membership)
                    with self.assertRaises(ValueError):
                        budget.processes([123])

    def test_dead_process_is_not_membership_evidence(self):
        for state in ["Z", "X", "x"]:
            with self.subTest(state=state):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    fixture = _make_fixture(tmp_path)
                    budget, _, task = fixture
                    (task / "stat").write_text(process_stat(state=state))
                    with self.assertRaises(ValueError):
                        budget.processes([123])

    def test_disappeared_process_is_not_passed(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, task = fixture
            (task / "stat").unlink()
            with self.assertRaises(FileNotFoundError):
                budget.processes([123])

    def test_reject_changes_within_witness(self):
        for changed in ["identity", "membership", "budget"]:
            with self.subTest(changed=changed):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    fixture = _make_fixture(tmp_path)
                    budget, group, task = fixture
                    original = Path.read_text
                    reads = 0

                    def read(path, *args, **kwargs):
                        nonlocal reads
                        text = original(path, *args, **kwargs)
                        if path == task / "cgroup":
                            reads += 1
                            if reads == 1:
                                if changed == "identity":
                                    (task / "stat").write_text(process_stat(started=43))
                                elif changed == "membership":
                                    (task / "cgroup").write_text("0::/outside.service\n")
                                else:
                                    (group / "memory.max").write_text("2147483648")
                        return text

                    resources.enter_context(mock.patch.object(Path, "read_text", read))
                    with self.assertRaises(ValueError):
                        budget.processes([123])

    def test_reject_missing_duplicate_or_invalid_scopes(self):
        for pids in [[], [123, 123], [0], [-1], [True]]:
            with self.subTest(pids=pids):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    fixture = _make_fixture(tmp_path)
                    budget, _, _ = fixture
                    with self.assertRaises(ValueError):
                        budget.processes(pids)

    def test_reject_corrupt_cpu_lists(self):
        for value in ["", "1-0", "0-1,1", "-1", "0-2-3", "a", "1048576"]:
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    parse_cpus(value)

    def test_cpu_ranges_and_cgroup_root_are_parsed(self):
        assert parse_cpus("0-2,4,6-7") == {0, 1, 2, 4, 6, 7}
        assert str(unified_path("0::/\n")) == "/"

    def test_reject_budget_at_global_root_or_outside_it(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, _ = fixture
            for path in (budget.cgroup_root, tmp_path):
                with self.assertRaises(ValueError):
                    ResourceBudget(
                        path,
                        1073741824,
                        frozenset({0, 1}),
                        cgroup_root=budget.cgroup_root,
                        proc_root=budget.proc_root,
                    )

    def test_declared_budget_cannot_mix_different_cpu_shapes(self):
        for memory, cpus, count in [
            (0, {0, 1}, 2),
            (-1, {0, 1}, 2),
            (True, {0, 1}, 2),
            (1.5, {0, 1}, 2),
            (1073741824, {0, 1, 2}, 2),
            (1073741824, {0}, 2),
            (1073741824, {0, True}, 2),
            (1073741824, {-1, 0}, 2),
            (1073741824, {0, 1}, True),
            (1073741824, {0, 1}, 2.0),
        ]:
            with self.subTest(memory=memory, cpus=cpus, count=count):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    fixture = _make_fixture(tmp_path)
                    budget, group, _ = fixture
                    with self.assertRaises(ValueError):
                        ResourceBudget(
                            group,
                            memory,
                            frozenset(cpus),
                            cpu_count=count,
                            cgroup_root=budget.cgroup_root,
                            proc_root=budget.proc_root,
                        )

    def test_explicit_affinity_mode_requires_every_observed_thread(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, _ = affinity_fixture(fixture)
            result = budget.processes([123])
            assert result["before"]["effective_cpus"] is None
            assert result["before"]["cpu_placement"] == "affinity"
            threads = result["witnesses"][0]["threads"]
            assert [thread["tid"] for thread in threads] == [123, 124]
            assert all(thread["cpus"] == [0, 1] for thread in threads)

    def test_cpuset_mode_never_silently_changes_to_affinity(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            _, group, _ = fixture
            (group / "cpuset.cpus.effective").unlink()
            with self.assertRaises(ValueError):
                fixture[0].processes([123])

    def test_one_invalid_thread_rejects_entire_affinity_witness(self):
        for file, value in [
            ("status", "Cpus_allowed_list: 0-15\n"),
            ("status", "Name: worker\n"),
            ("cgroup", "0::/outside.service\n"),
            ("stat", process_stat(pid=124, state="Z")),
        ]:
            with self.subTest(file=file, value=value):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    fixture = _make_fixture(tmp_path)
                    budget, _, task = affinity_fixture(fixture)
                    (task / "task/124" / file).write_text(value)
                    with self.assertRaises(ValueError):
                        budget.processes([123])

    def test_disappeared_thread_is_unknown(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, task = affinity_fixture(fixture)
            (task / "task/124/status").unlink()
            with self.assertRaises(FileNotFoundError):
                budget.processes([123])

    def test_affinity_violation_retains_offending_thread_identity(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, task = affinity_fixture(fixture)
            (task / "task/124/status").write_text("Cpus_allowed_list: 0-15\n")
            with self.assertRaises(BudgetViolation) as caught:
                budget.processes([123])
            assert caught.exception.evidence == dict(
                pid=123,
                start_ticks=42,
                process_name="worker ) name",
                tid=124,
                thread_start_ticks=42,
                thread_name="worker ) name",
                cgroup="/private.slice/worker.service",
                expected_cgroup="/private.slice",
                cpus=list(range(16)),
                declared_cpus=[0, 1],
            )

    def test_reject_threads_created_during_affinity_observation(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, task = affinity_fixture(fixture)
            original = Path.read_text

            def read(path, *args, **kwargs):
                text = original(path, *args, **kwargs)
                if path == task / "task/123/status":
                    (task / "task/125").mkdir(exist_ok=True)
                return text

            resources.enter_context(mock.patch.object(Path, "read_text", read))
            with self.assertRaises(ValueError):
                budget.processes([123])

    def test_reject_affinity_changed_within_thread_observation(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, task = affinity_fixture(fixture)
            original = Path.read_text

            def read(path, *args, **kwargs):
                text = original(path, *args, **kwargs)
                if path == task / "task/123/status":
                    path.write_text("Cpus_allowed_list: 0-15\n")
                return text

            resources.enter_context(mock.patch.object(Path, "read_text", read))
            with self.assertRaises(BudgetViolation) as caught:
                budget.processes([123])

            assert caught.exception.evidence["tid"] == 123
            assert caught.exception.evidence["cpus"] == list(range(16))

    def test_individually_pinned_threads_remain_inside_shared_cpu_budget(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, task = affinity_fixture(fixture)
            (task / "task/124/status").write_text("Cpus_allowed_list: 0\n")
            result = budget.processes([123])
            assert result["before"]["declared_cpu_set"] == [0, 1]
            assert result["before"]["cpu_quota"] == 200000
            assert result["witnesses"][0]["threads"][1]["cpus"] == [0]

    def test_subtree_witness_includes_detached_scope(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, _, _, _ = subtree_fixture(fixture)
            result = budget.witness_all_members([123, 456])
            assert {w["pid"] for w in result["witnesses"]} == {123, 456}
            assert result["membership"][456] == "/private.slice/detached.scope"

    def test_subtree_cannot_succeed_with_missing_required_scope(self):
        for required in [[], [123, 123], [True], [0]]:
            with self.subTest(required=required):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    fixture = _make_fixture(tmp_path)
                    budget, _, _, _, _ = subtree_fixture(fixture)
                    with self.assertRaises(ValueError):
                        budget.witness_all_members(required)

    def test_subtree_cannot_succeed_when_required_pid_has_disappeared(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, _, _, _ = subtree_fixture(fixture)
            with self.assertRaises(FileNotFoundError):
                budget.witness_all_members([789])

    def test_subtree_rejects_process_escape_from_listed_group(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, _, _, helper = subtree_fixture(fixture)
            (helper / "cgroup").write_text("0::/outside.scope\n")
            with self.assertRaises(ValueError):
                budget.witness_all_members([123])

    def test_subtree_rejects_duplicate_membership(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, group, _, _, _ = subtree_fixture(fixture)
            (group / "cgroup.procs").write_text("123\n")
            with self.assertRaises(ValueError):
                budget.witness_all_members([123])

    def test_subtree_rejects_process_that_exits_before_witness(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, _, _, helper = subtree_fixture(fixture)
            (helper / "stat").unlink()
            with self.assertRaises(FileNotFoundError):
                budget.witness_all_members([123])

    def test_subtree_rejects_membership_change_after_process_check(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, _, detached, _ = subtree_fixture(fixture)
            original = Path.read_text
            reads = 0

            def read(path, *args, **kwargs):
                nonlocal reads
                text = original(path, *args, **kwargs)
                if path.name == "cpu.stat":
                    reads += 1
                    if reads == 4:
                        (detached / "cgroup.procs").write_text("")
                return text

            resources.enter_context(mock.patch.object(Path, "read_text", read))
            with self.assertRaises(ValueError):
                budget.witness_all_members([123])

    def test_live_required_root_outside_parent_is_violation_not_missing_observation(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, _, detached, helper = subtree_fixture(fixture)
            (detached / "cgroup.procs").write_text("")
            (helper / "cgroup").write_text("0::/outside.scope\n")
            with self.assertRaises(BudgetViolation):
                budget.witness_all_members([123, 456])

    def test_exited_root_is_unknown_not_constraint_violation(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            fixture = _make_fixture(tmp_path)
            budget, _, _, _, helper = subtree_fixture(fixture)
            (helper / "stat").write_text(process_stat(pid=456, state="Z"))
            with self.assertRaises(ObservationChanged):
                budget.witness_all_members([123, 456])
