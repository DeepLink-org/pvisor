"""Coordinator tests only: no KVM, sysfs writes or real systemd services."""

import copy
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

import memory_scale as scale


def valid(mode="baseline", n=2, scanner="0"):
    config = dict(
        example="/frozen/worker",
        example_sha256="b" * 64,
        rootfs="/rootfs",
        firmware="/firmware",
        output="/short/t0",
        vms=n,
        mode=mode,
        pattern="repeated",
        dedup=mode in ("baseline", "ksm"),
        seed=7,
        settle_ms=500,
        ksm_wait_seconds=2,
        cgroup="/sys/fs/cgroup/owned.service",
    )
    accounting = dict(
        cgroup=config["cgroup"],
        process_errors=[],
        processes=[
            dict(smaps={"raw": "Pss: 12 kB\nKSM: 0 kB\n"}, smaps_totals_bytes={"Pss": 12288})
        ],
        counters={
            name: {"raw": value}
            for name, value in {
                "memory.max": "2147483648",
                "memory.swap.max": "0",
                "cpu.max": "400000 100000",
                "memory.current": "100",
                "memory.peak": "200",
                "memory.swap.current": "0",
                "pids.current": "2",
                "memory.stat": "anon 50\nfile 40\nkernel 10\n",
                "memory.events": "oom 0\noom_kill 0\n",
                "memory.events.local": "oom 0\noom_kill 0\n",
                "cpu.stat": "usage_usec 100\n",
                "memory.pressure": "some avg10=0 total=0",
                "cpu.pressure": "some avg10=0 total=0",
            }.items()
        },
        ksm={"run": {"error": "not readable"} if scanner is None else {"raw": scanner + "\n"}},
    )
    restored_mode = mode in ("baseline", "ksm")
    write_mode = restored_mode or mode == "fresh"
    expected = {(0, 0): "a" * 64}
    expected.update(
        {
            (i, p): f"{i * 100 + p:064x}"
            for i in range(1, n + 1)
            for p in ((0, 25, 100) if write_mode else (0,))
        }
    )
    names = (
        ["ready", "cow25", "cow100", "after_exit"]
        if write_mode
        else ["ready", "offloaded0", "resumed0", "offloaded1", "resumed1", "after_exit"]
    )
    if mode == "ksm":
        names = [
            "ready",
            "dynamic_private_before_wait",
            "dynamic_private_after_wait",
            "cow25",
            "cow100",
            "after_exit",
        ]
    report = dict(
        schema="pvisor-memory-scale/v1",
        correctness="passed",
        conditions={
            k: v for k, v in config.items() if k not in ("example", "example_sha256", "cgroup")
        },
        profile=dict(
            memory_mib=256,
            cpus=1,
            payload_bytes=64 * 1024**2,
            page_bytes=4096,
            max_live_vms=4,
            deadline_seconds=180,
        ),
        source={"binary_sha256": config["example_sha256"]},
        cleanup={"all_reaped": True},
        expected_digests=[dict(instance=i, percent=p, digest=d) for (i, p), d in expected.items()],
        before=copy.deepcopy(accounting),
        after=copy.deepcopy(accounting),
        ksm_scan_window=dict(
            seconds=2,
            deadline_is_not_product_failure=True,
            before=copy.deepcopy(accounting),
            after=copy.deepcopy(accounting),
        ),
        phases=[],
        checks=[],
        guests=[],
    )
    report["conditions"]["private_baselines"] = False
    for index, name in enumerate(names):
        ids = list(range(2 if name == "after_exit" else 1, n + 1))
        report["phases"].append(
            dict(
                name=name,
                elapsed_ms=index,
                paused=True,
                offloaded=name.startswith("offloaded"),
                instances=ids,
                heartbeat=[100] * len(ids),
                heartbeat_stable=True,
                accounting=copy.deepcopy(accounting),
            )
        )

    def check(name, evidence):
        report["checks"].append(dict(name=name, passed=True, evidence=evidence))

    heartbeat = {i: 10 for i in range(n + 1)}

    def phase(name):
        row = next(p for p in report["phases"] if p["name"] == name)
        row["heartbeat"] = [heartbeat[i] for i in row["instances"]]
        if not row["offloaded"]:
            for i in row["instances"]:
                before = heartbeat[i]
                heartbeat[i] += 1
                check(
                    "resume_heartbeat_progress",
                    dict(phase=name, instance=i, before=before, after=heartbeat[i]),
                )

    def ack(token, op, i, p=0, oracle=None, restored=None, advance=True):
        slot = i if restored is None else restored
        heartbeat[slot] += int(advance)
        return dict(
            token=token,
            op=op,
            instance=i,
            percent=p,
            digest=expected[(i if oracle is None else oracle, p)],
            heartbeat=heartbeat[slot],
            read_ms=1,
        )

    check("group_budget", {})
    if restored_mode:
        check("producer_payload", ack("producer-ready", "read", 0))
        report["checkpoint"] = dict(
            snapshot_id="c" * 64,
            store="/checkpoint",
            source_run_id="run0",
            source_attempt_id="attempt0",
            created_at_unix_ms=1,
            ram_storage="raw",
        )
        receipt = dict(request_id="memory-scale-baseline", checkpoint=report["checkpoint"])
        check("producer_reaped_before_restore", receipt)
        report["producer_final_heartbeat"] = heartbeat[0]
    for i in range(1, n + 1):
        if restored_mode:
            heartbeat[i] = heartbeat[0]
            check(
                "restored_original_and_heartbeat",
                ack(f"original-{i}", "read", 0, oracle=0, restored=i),
            )
        check("ready_full_digest", ack(f"prepare-{i}", "prepare", i))
    phase("ready")
    if mode == "ksm":
        report["dynamic_ksm_scan_window"] = dict(
            seconds=2,
            deadline_is_not_product_failure=True,
            merging_required=False,
            before=copy.deepcopy(accounting),
            after=copy.deepcopy(accounting),
        )
        for i in range(1, n + 1):
            check("dynamic_private_full_digest", ack(f"duplicate-{i}", "duplicate", i))
        phase("dynamic_private_before_wait")
        for i in range(1, n + 1):
            check("dynamic_private_after_wait_full_digest", ack(f"dynamic-readback-{i}", "read", i))
        phase("dynamic_private_after_wait")
    if restored_mode:
        for i in range(1, n + 1):
            check(
                "private_offload_rejected_and_healthy",
                dict(
                    instance=i,
                    error="restored private COW RAM",
                    ack=ack(f"rejected-offload-{i}", "read", i),
                ),
            )
    if write_mode:
        for p in (25, 100):
            for index in range(n):
                i = index + 1
                check("cow_full_digest", ack(f"cow{p}-{i}", "mutate", i, p))
                for peer in range(index + 2, n + 1):
                    check(
                        "peer_write_isolation",
                        ack(f"isolation-{p}-{index}-{peer}", "read", peer, 0 if p == 25 else 25),
                    )
            phase(f"cow{p}")
    else:
        for cycle in range(2):
            for i in range(1, n + 1):
                check(
                    "offload_receipt",
                    dict(
                        instance=i,
                        cycle=cycle,
                        elapsed_ms=1,
                        memory={"backed_bytes": 256 * 1024**2},
                    ),
                )
            phase(f"offloaded{cycle}")
            for i in range(1, n + 1):
                parked = heartbeat[i]
                readback = ack(f"readback-{cycle}-{i}", "read", i, advance=False)
                heartbeat[i] += 1
                check(
                    "offload_resume_full_digest_and_heartbeat",
                    dict(
                        cycle=cycle,
                        elapsed_ms=1,
                        ack=readback,
                        parked_heartbeat=parked,
                        resumed_heartbeat=heartbeat[i],
                    ),
                )
            phase(f"resumed{cycle}")
    check("one_vm_cancelled_and_reaped", {"instance": 1})
    p = 100 if write_mode else 0
    for i in range(2, n + 1):
        check("survivor_full_digest", ack(f"survivor-{i}", "read", i, p))
    phase("after_exit")
    for i in range(2, n + 1):
        check("orderly_exit_digest", ack(f"exit-{i}", "exit", i, p))
    check("cleanup_all_reaped", {"all_reaped": True})
    for i in ([0] if restored_mode else []) + list(range(1, n + 1)):
        result = dict(
            run_id=f"run{i}",
            attempt_id=f"attempt{i}",
            started_at_unix_ms=1 if i == 0 else 3,
            finished_at_unix_ms=2 if i == 0 else 10,
            state="hibernated" if i == 0 else "cancelled" if i == 1 else "completed",
        )
        if i == 0:
            result["value"] = receipt
        elif i >= 2:
            result["exit_code"] = 0
        report["guests"].append(dict(instance=i, result=result))
    return report, config


class MemoryScaleTests(unittest.TestCase):
    def test_exact_worker_schema_accepted(self):
        for mode in scale.MODES:
            for n in [1, 2, 4]:
                with self.subTest(mode=mode, n=n):
                    report, config = valid(mode, n)
                    assert scale.validate_report(report, config)["merging_claim"] is False
                    cmd = scale.worker_command(config)
                    assert cmd[cmd.index("--dedup") + 1] == (
                        "true" if mode in ("baseline", "ksm") else "false"
                    )
                    assert "--private-baselines" not in cmd
                    assert cmd[cmd.index("--vms") + 1] == str(n)
                    progress = [
                        c for c in report["checks"] if c["name"] == "resume_heartbeat_progress"
                    ]
                    assert len(progress) == (6 if mode == "ksm" else 4) * n - 1
                    offload_resumes = [
                        c["evidence"]
                        for c in report["checks"]
                        if c["name"] == "offload_resume_full_digest_and_heartbeat"
                    ]
                    assert len(offload_resumes) == (0 if mode in ("baseline", "ksm") else 2 * n)
                    assert all(
                        e["ack"]["heartbeat"] == e["parked_heartbeat"] < e["resumed_heartbeat"]
                        for e in offload_resumes
                    )

    def test_phase_resume_progress_requires_exact_strict_proofs(self):
        for mode in scale.MODES:
            for mutation in [
                "missing",
                "duplicate",
                "equal",
                "regressed",
                "no_before",
                "no_after",
                "boolean",
                "phase",
                "instance",
                "frozen",
            ]:
                with self.subTest(mode=mode, mutation=mutation):
                    report, config = valid(mode)
                    check = next(
                        c for c in report["checks"] if c["name"] == "resume_heartbeat_progress"
                    )
                    evidence = check["evidence"]
                    if mutation == "missing":
                        report["checks"].remove(check)
                    elif mutation == "duplicate":
                        report["checks"].append(copy.deepcopy(check))
                    elif mutation == "equal":
                        evidence["after"] = evidence["before"]
                    elif mutation == "regressed":
                        evidence["after"] = evidence["before"] - 1
                    elif mutation == "no_before":
                        del evidence["before"]
                    elif mutation == "no_after":
                        del evidence["after"]
                    elif mutation == "boolean":
                        evidence["before"] = True
                    elif mutation == "phase":
                        evidence["phase"] = "after_exit"
                    elif mutation == "instance":
                        evidence["instance"] = 99
                    else:
                        evidence["before"] -= 1
                    with self.assertRaises(ValueError):
                        scale.validate_report(report, config)

    def test_offload_resume_requires_strict_bound_heartbeat_proofs(self):
        for mode in ["raw", "compressed"]:
            for mutation in [
                "missing",
                "duplicate",
                "equal",
                "regressed",
                "no_parked",
                "no_resumed",
                "boolean",
                "parked",
                "ack_above_resumed",
            ]:
                with self.subTest(mode=mode, mutation=mutation):
                    report, config = valid(mode)
                    check = next(
                        c
                        for c in report["checks"]
                        if c["name"] == "offload_resume_full_digest_and_heartbeat"
                    )
                    evidence = check["evidence"]
                    if mutation == "missing":
                        report["checks"].remove(check)
                    elif mutation == "duplicate":
                        report["checks"].append(copy.deepcopy(check))
                    elif mutation == "equal":
                        evidence["resumed_heartbeat"] = evidence["parked_heartbeat"]
                    elif mutation == "regressed":
                        evidence["resumed_heartbeat"] = evidence["parked_heartbeat"] - 1
                    elif mutation == "no_parked":
                        del evidence["parked_heartbeat"]
                    elif mutation == "no_resumed":
                        del evidence["resumed_heartbeat"]
                    elif mutation == "boolean":
                        evidence["parked_heartbeat"] = True
                    elif mutation == "parked":
                        evidence["parked_heartbeat"] -= 1
                    else:
                        evidence["ack"]["heartbeat"] = evidence["resumed_heartbeat"] + 1
                    with self.assertRaises(ValueError):
                        scale.validate_report(report, config)

    def test_same_iteration_command_ack_is_valid_but_restore_readiness_stays_strict(self):
        for mode in ["baseline", "ksm"]:
            with self.subTest(mode=mode):
                report, config = valid(mode)
                original = next(
                    c["evidence"]
                    for c in report["checks"]
                    if c["name"] == "restored_original_and_heartbeat"
                )
                ready = next(
                    c["evidence"] for c in report["checks"] if c["name"] == "ready_full_digest"
                )
                ready["heartbeat"] = original["heartbeat"]
                scale.validate_report(report, config)
                original["heartbeat"] = report["producer_final_heartbeat"]
                with self.assertRaisesRegex(ValueError, "strict readiness"):
                    scale.validate_report(report, config)

    def test_command_ack_can_equal_separately_proven_resume_progress(self):
        for mode in ["baseline", "ksm"]:
            with self.subTest(mode=mode):
                report, config = valid(mode)
                phase = "dynamic_private_after_wait" if mode == "ksm" else "ready"
                progressed = next(
                    c["evidence"]
                    for c in report["checks"]
                    if c["name"] == "resume_heartbeat_progress"
                    and c["evidence"]["phase"] == phase
                    and c["evidence"]["instance"] == 1
                )
                rejected = next(
                    c["evidence"]["ack"]
                    for c in report["checks"]
                    if c["name"] == "private_offload_rejected_and_healthy"
                )
                rejected["heartbeat"] = progressed["after"]
                scale.validate_report(report, config)
                rejected["heartbeat"] -= 1
                with self.assertRaisesRegex(ValueError, "regressed"):
                    scale.validate_report(report, config)

    def test_mutually_exclusive_advice(self):
        for mode in ["raw", "compressed"]:
            with self.subTest(mode=mode):
                _, config = valid(mode)
                config["dedup"] = True
                with self.assertRaisesRegex(ValueError, "separate"):
                    scale.worker_command(config)

    def test_incomplete_reports_rejected(self):
        for mutation in [
            "phase",
            "checks",
            "cleanup",
            "oracle",
            "guest",
            "binary",
            "barrier",
            "snapshot",
            "heartbeat",
            "condition",
            "offload",
        ]:
            with self.subTest(mutation=mutation):
                report, config = valid("raw" if mutation == "offload" else "baseline")
                if mutation == "phase":
                    report["phases"].pop()
                elif mutation == "checks":
                    report["checks"] = [
                        c for c in report["checks"] if c["name"] != "ready_full_digest"
                    ]
                elif mutation == "cleanup":
                    report["cleanup"]["all_reaped"] = False
                elif mutation == "oracle":
                    report["expected_digests"].pop()
                elif mutation == "guest":
                    report["guests"][0]["result"]["state"] = "running"
                elif mutation == "binary":
                    report["source"]["binary_sha256"] = "wrong"
                elif mutation == "barrier":
                    report["phases"][0]["heartbeat_stable"] = False
                elif mutation == "snapshot":
                    report["checkpoint"]["source_run_id"] = "another-run"
                elif mutation == "heartbeat":
                    next(c for c in report["checks"] if c["name"] == "ready_full_digest")[
                        "evidence"
                    ]["heartbeat"] = 1
                elif mutation == "condition":
                    report["conditions"]["seed"] += 1
                else:
                    report["checks"] = [
                        c for c in report["checks"] if c["name"] != "offload_receipt"
                    ]
                with self.assertRaises(ValueError):
                    scale.validate_report(report, config)

    def test_per_phase_budget_and_accounting(self):
        for field, value in [
            ("memory.max", "max"),
            ("memory.swap.max", "1"),
            ("cpu.max", "200000 100000"),
            ("memory.swap.current", "1"),
            ("memory.events", "oom 1\noom_kill 0"),
            ("memory.events.local", "oom 0\noom_kill 1"),
            ("memory.stat", "anon 1\nfile 2"),
            ("cpu.stat", ""),
            ("memory.pressure", ""),
        ]:
            with self.subTest(field=field, value=value):
                report, config = valid()
                report["phases"][1]["accounting"]["counters"][field]["raw"] = value
                with self.assertRaises(ValueError):
                    scale.validate_report(report, config)

    def test_same_group_and_cpu_monotonicity(self):
        report, config = valid()
        report["phases"][0]["accounting"]["cgroup"] = "/escaped"
        with self.assertRaisesRegex(ValueError, "escaped"):
            scale.validate_report(report, config)
        report, config = valid()
        report["after"]["counters"]["cpu.stat"]["raw"] = "usage_usec 1"
        with self.assertRaisesRegex(ValueError, "CPU"):
            scale.validate_report(report, config)

    def test_scanner_observations_never_claim_group_savings(self):
        for state in [None, "0", "1", "2"]:
            with self.subTest(state=state):
                report, config = valid(scanner=state)
                observation = scale.validate_report(report, config)
                assert observation["scanner_state"] == ("unknown" if state is None else state)
                assert observation["merging_claim"] is False
                assert "global" in observation["ksm_attribution"]

    def test_scanner_phase_consistency(self):
        report, config = valid()
        report["phases"][1]["accounting"]["ksm"]["run"]["raw"] = "1"
        with self.assertRaisesRegex(ValueError, "scanner state changed"):
            scale.validate_report(report, config)

    def test_ksm_mode_is_a_restored_advice_pair(self):
        for advice in [False, True]:
            for n in [1, 2, 4]:
                with self.subTest(advice=advice, n=n):
                    args = scale.parser().parse_args(
                        ["--rootfs", "/r", "--firmware", "/f", "--output", "/o", "--modes", "ksm"]
                    )
                    assert len(scale.cells(args)) == 18
                    report, config = valid("ksm", n)
                    report["conditions"]["dedup"] = config["dedup"] = advice
                    scale.validate_report(report, config)
                    cmd = scale.worker_command(config)
                    assert cmd[cmd.index("--mode") + 1] == "ksm"
                    assert cmd[cmd.index("--dedup") + 1] == str(advice).lower()
                    assert [g["instance"] for g in report["guests"]] == list(range(n + 1))

    def test_incomplete_dynamic_ksm_evidence_rejected(self):
        for mutation in [
            "window",
            "seconds",
            "deadline",
            "merging",
            "before",
            "after",
            "phase",
            "duplicate",
            "readback",
            "token",
            "op",
            "digest",
        ]:
            with self.subTest(mutation=mutation):
                report, config = valid("ksm")
                window = report["dynamic_ksm_scan_window"]
                if mutation == "window":
                    del report["dynamic_ksm_scan_window"]
                elif mutation == "seconds":
                    window["seconds"] += 1
                elif mutation == "deadline":
                    window["deadline_is_not_product_failure"] = False
                elif mutation == "merging":
                    window["merging_required"] = True
                elif mutation in ("before", "after"):
                    del window[mutation]
                elif mutation == "phase":
                    report["phases"].pop(1)
                elif mutation in ("duplicate", "readback"):
                    name = (
                        "dynamic_private_full_digest"
                        if mutation == "duplicate"
                        else "dynamic_private_after_wait_full_digest"
                    )
                    report["checks"].remove(next(c for c in report["checks"] if c["name"] == name))
                else:
                    ack = next(
                        c for c in report["checks"] if c["name"] == "dynamic_private_full_digest"
                    )["evidence"]
                    ack[mutation] = {"token": "wrong-token", "op": "read", "digest": "f" * 64}[
                        mutation
                    ]
                with self.assertRaises(ValueError):
                    scale.validate_report(report, config)

    def test_dynamic_ksm_cpu_accounting_uses_actual_temporal_order(self):
        report, config = valid("ksm")
        initial = report["ksm_scan_window"]
        dynamic = report["dynamic_ksm_scan_window"]
        phases = report["phases"]
        ordered = [
            report["before"],
            initial["before"],
            initial["after"],
            phases[0]["accounting"],
            phases[1]["accounting"],
            dynamic["before"],
            dynamic["after"],
            phases[2]["accounting"],
            phases[3]["accounting"],
            phases[4]["accounting"],
            phases[5]["accounting"],
            report["after"],
        ]
        for i, snapshot in enumerate(ordered):
            snapshot["counters"]["cpu.stat"]["raw"] = f"usage_usec {100 + i}"
        scale.validate_report(report, config)
        # Each window endpoint must be checked against its surrounding barriers.
        dynamic["before"]["counters"]["cpu.stat"]["raw"] = "usage_usec 103"
        with self.assertRaisesRegex(ValueError, "nonmonotonic group CPU"):
            scale.validate_report(report, config)
        dynamic["before"]["counters"]["cpu.stat"]["raw"] = "usage_usec 105"
        dynamic["after"]["counters"]["cpu.stat"]["raw"] = "usage_usec 108"
        with self.assertRaisesRegex(ValueError, "nonmonotonic group CPU"):
            scale.validate_report(report, config)

    def test_dynamic_ksm_window_accounting_is_mandatory(self):
        for endpoint in ["before", "after"]:
            for mutation in ["scanner", "group", "oom", "budget"]:
                with self.subTest(endpoint=endpoint, mutation=mutation):
                    report, config = valid("ksm")
                    accounting = report["dynamic_ksm_scan_window"][endpoint]
                    if mutation == "scanner":
                        accounting["ksm"]["run"]["raw"] = "1"
                    elif mutation == "group":
                        accounting["cgroup"] = "/escaped"
                    elif mutation == "oom":
                        accounting["counters"]["memory.events"]["raw"] = "oom 0\noom_kill 1"
                    else:
                        accounting["counters"]["cpu.max"]["raw"] = "200000 100000"
                    with self.assertRaises(ValueError):
                        scale.validate_report(report, config)

    def test_restored_modes_do_not_require_merging(self):
        for mode in ["baseline", "ksm"]:
            for state in [None, "0", "1", "2"]:
                with self.subTest(mode=mode, state=state):
                    report, config = valid(mode, scanner=state)
                    assert scale.validate_report(report, config)["merging_claim"] is False

    def test_input_manifest_hashes_symlink_target(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            (tmp_path / "file").write_bytes(b"firmware")
            (tmp_path / "link").symlink_to("file")
            manifest = scale.inventory(tmp_path)
            link = next(r for r in manifest if r["path"] == "link")
            assert link["target"] == "file"
            assert link["resolved_sha256"] == scale.digest(tmp_path / "file")

    def test_fresh_independent_group_requires_write_isolation_without_snapshot_producer(self):
        report, config = valid("fresh", 4, scanner="1")
        config["dedup"] = report["conditions"]["dedup"] = True
        for guest in report["guests"]:
            guest["result"]["output"] = {
                "stderr": "VM RAM dedup advice installation: accepted_bytes=536870912"
            }
        report["phases"][0]["accounting"]["processes"] *= 4
        for process in report["phases"][0]["accounting"]["processes"]:
            process["smaps"]["raw"] += "VmFlags: rd wr mr mw me ac mg\n"
        assert scale.validate_report(report, config)["merging_claim"] is False
        assert all(g["instance"] != 0 for g in report["guests"])
        report["checks"] = [c for c in report["checks"] if c["name"] != "peer_write_isolation"]
        with self.assertRaises(ValueError):
            scale.validate_report(report, config)

    def test_declared_four_gib_ceiling_is_checked_in_every_snapshot(self):
        report, config = valid("fresh", 4, scanner="1")
        config["group_memory_max"] = report["conditions"]["group_memory_max"] = 4294967296
        snapshots = [
            report["before"],
            report["after"],
            report["ksm_scan_window"]["before"],
            report["ksm_scan_window"]["after"],
            *(p["accounting"] for p in report["phases"]),
        ]
        for snapshot in snapshots:
            snapshot["counters"]["memory.max"]["raw"] = "4294967296"
        scale.validate_report(report, config)
        snapshots[-1]["counters"]["memory.max"]["raw"] = "2147483648"
        with self.assertRaisesRegex(ValueError, "memory/swap budget"):
            scale.validate_report(report, config)

    def test_budget_cannot_silently_differ_from_planned_condition(self):
        report, config = valid("fresh", 4, scanner="1")
        report["conditions"]["group_memory_max"] = 4294967296
        with self.assertRaisesRegex(ValueError, "budget condition mismatch"):
            scale.validate_report(report, config)

    def test_fresh_ksm_rejects_skipped_shared_ram_advice(self):
        report, config = valid("fresh", 4, scanner="1")
        config["dedup"] = report["conditions"]["dedup"] = True
        for guest in report["guests"]:
            guest["result"]["output"] = {
                "stderr": "VM RAM dedup advice installation: accepted_bytes=0"
            }
        with self.assertRaisesRegex(ValueError, "accepted advice and mergeable RAM"):
            scale.validate_report(report, config)

    def test_streamed_smaps_keeps_full_evidence_and_rejects_tampering(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            import hashlib

            raw, config = valid()
            accounting = raw["before"]
            process = accounting["processes"][0]
            text = process["smaps"]["raw"].encode()
            directory = tmp_path / "smaps"
            directory.mkdir()
            artifact = directory / "0000-123.smaps"
            artifact.write_bytes(text)
            process["smaps"] = dict(
                file=artifact.name,
                bytes=len(text),
                sha256=hashlib.sha256(text).hexdigest(),
                capture="streamed-synced-cache-discarded",
            )
            scale.validate_accounting(accounting, config["cgroup"], worker=tmp_path / "w")
            process["smaps_totals_bytes"]["Pss"] += 1024
            with self.assertRaisesRegex(ValueError, "totals evidence"):
                scale.validate_accounting(accounting, config["cgroup"], worker=tmp_path / "w")
            process["smaps_totals_bytes"]["Pss"] -= 1024
            artifact.write_bytes(text.replace(b"12", b"13"))
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                scale.validate_accounting(accounting, config["cgroup"], worker=tmp_path / "w")
            process["smaps"]["file"] = "../escape"
            with self.assertRaisesRegex(ValueError, "unsafe smaps"):
                scale.smaps_text(process, tmp_path / "w")

    def test_process_rollup_retains_fractional_pss_lost_in_per_vma_text(self):
        raw, config = valid()
        accounting = raw["before"]
        process = accounting["processes"][0]
        process["smaps"] = {"raw": "Pss: 0 kB\nPss: 0 kB\n"}
        process["smaps_vma_totals_bytes"] = {"Pss": 0}
        process["smaps_rollup"] = {"raw": "Pss: 1 kB\n"}
        process["smaps_totals_bytes"] = {"Pss": 1024}
        scale.validate_accounting(accounting, config["cgroup"])
        process["smaps_totals_bytes"]["Pss"] = 0
        with self.assertRaisesRegex(ValueError, "rollup evidence"):
            scale.validate_accounting(accounting, config["cgroup"])
