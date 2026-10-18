#!/usr/bin/env python3
"""Offline filesystem workload design; deliberately contains no executor.

Quick is a 19-case catalog, not a measured benchmark. Curves exposes the same
anchors and deferred one-axis extension specifications, never a Cartesian sweep.
Each selected case/arm/pair needs its own fresh environment for three rounds.
"""

import argparse
import copy
import hashlib
import json
import random
import sys
from pathlib import Path

MIB = 1 << 20
COMPARISONS = ("guest-writeback", "host-writeback", "lower-read-cache")
GUEST_WB = "PVISOR_VM_FS_WRITEBACK"
HOST_WB = "PVISOR_VM_FS_HOST_WRITEBACK"
DATA = "PVISOR_VM_FS_LOWER_DATA_CACHE"
FIXED_ENV = {
    GUEST_WB: "0",
    HOST_WB: "0",
    DATA: "0",
    "PVISOR_VM_GUEST_CACHE": "1",
    "PVISOR_VM_FS_DIRECTORY_CACHE_CAPACITY": "256",
    "PVISOR_VM_FS_READONLY_FLUSH_ELISION": "0",
    "PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE": "0",
    "PVISOR_VM_FS_SHARED_OPEN_QUALIFICATION": "0",
    "PVISOR_IMMUTABLE_LOWER_NEGATIVE_CACHE": "0",
    "PVISOR_VM_FS_EXCLUSIVE_UPPER_ABSENCE_CACHE": "0",
    "PVISOR_FS_PROFILE": "0",
}


def require(value, message):
    if not value:
        raise ValueError(message)


def _rng(seed, domain):
    digest = hashlib.sha256(f"filesystem-v2:{seed}:{domain}".encode()).digest()
    return random.Random(int.from_bytes(digest, "big"))


def _applicability(reads_lower=False, read_control=False):
    return {
        "guest-writeback": dict(
            eligible=True,
            role="target" if not read_control else "negative_control",
            reason="writes use guest WB; read-only anchors remain controls",
        ),
        "host-writeback": dict(
            eligible=True,
            role="target" if not read_control else "negative_control",
            reason="writes use host WB; read-only anchors remain controls",
        ),
        "lower-read-cache": dict(
            eligible=reads_lower or read_control,
            role="target"
            if reads_lower
            else "negative_control"
            if read_control
            else "not_applicable",
            reason="authenticated Immutable lower reads required; upper writes remain fallback"
            if reads_lower
            else "metadata-only control; no data-cache gain expected"
            if read_control
            else "pure upper-write case has no lower data reads",
        ),
    }


def case_catalog(guest_memory_mib=512):
    """Return intended operations and oracles, without creating any fixture."""
    require(
        type(guest_memory_mib) is int and guest_memory_mib > 0,
        "guest memory must be a positive integer MiB",
    )
    ram = guest_memory_mib * MIB
    cases = []

    def add(
        id_,
        name,
        layer,
        operation,
        parameters,
        *,
        lower=False,
        control=False,
        sync="not_applicable",
        oracle="exact lengths, deterministic bytes and namespace membership",
    ):
        cases.append(
            dict(
                id=id_,
                name=name,
                layer=layer,
                operation=operation,
                parameters=parameters,
                synchronization=sync,
                oracle=oracle,
                comparison_applicability=_applicability(lower, control),
                executor="not_implemented",
                fixture="not_implemented",
            )
        )

    add(
        "M01",
        "positive lookup",
        "mechanism",
        "stat_existing",
        dict(paths=2048, operations=2048),
        control=True,
        oracle="expected file types, sizes and identities",
    )
    add(
        "M02",
        "negative lookup",
        "mechanism",
        "stat_missing",
        dict(paths=2048, operations=2048),
        control=True,
        oracle="each absent path returns ENOENT; existing sentinel remains readable",
    )
    add(
        "M03",
        "directory enumeration",
        "mechanism",
        "readdir",
        dict(directories=1, entries=2048),
        control=True,
        oracle="exact names, types and unique entry inventory",
    )
    add(
        "M04",
        "sequential reopen read",
        "mechanism",
        "reopen_pread_close",
        dict(
            file_count=1,
            working_set_bytes=4 * MIB,
            logical_read_bytes_per_pass=4 * MIB,
            block_bytes=65536,
            reopen_passes=3,
        ),
        lower=True,
        control=True,
    )
    add(
        "M05",
        "random read",
        "mechanism",
        "random_pread",
        dict(file_count=1, working_set_bytes=4 * MIB, logical_read_bytes=4 * MIB, block_bytes=4096),
        lower=True,
        control=True,
    )
    for id_, block in [("M06", 1024), ("M07", MIB)]:
        add(
            id_,
            f"sequential {block}-byte write",
            "mechanism",
            "sequential_pwrite",
            dict(file_count=1, workers=1, logical_write_bytes=4 * MIB, block_bytes=block),
            sync="final_file_fsync_checked_close_and_directory_fsync",
        )
    add(
        "M08",
        "random 4 KiB overwrite",
        "mechanism",
        "random_pwrite_existing_upper",
        dict(
            file_count=1,
            workers=1,
            working_set_bytes=4 * MIB,
            logical_write_bytes=4 * MIB,
            block_bytes=4096,
        ),
        sync="final_file_fsync_checked_close_and_directory_fsync",
    )
    add(
        "M09",
        "small-file write and close",
        "mechanism",
        "create_write_checked_close",
        dict(
            file_count=256,
            workers=1,
            bytes_per_file=65536,
            block_bytes=65536,
            logical_write_bytes=16 * MIB,
        ),
        sync="write_close_response_clock_then_separate_reopen_file_and_directory_fsync",
    )
    add(
        "M10",
        "fsync each 4 KiB write",
        "mechanism",
        "sequential_pwrite_fsync",
        dict(
            file_count=1,
            workers=1,
            logical_write_bytes=MIB,
            block_bytes=4096,
            periodic_fsync_every_ops=1,
        ),
        sync="each_write_file_fsync_then_checked_close_and_directory_fsync",
    )
    add(
        "M11",
        "foreground read with independent background writer",
        "mechanism",
        "concurrent_read_write",
        dict(
            file_count=2,
            workers=2,
            shared_inode=False,
            logical_read_bytes=4 * MIB,
            logical_write_bytes=4 * MIB,
            block_bytes=4096,
        ),
        lower=True,
        sync="reader_completion_and_writer_final_file_and_directory_fsync",
    )
    add(
        "M12",
        "create/write/fsync/close/rename/unlink",
        "mechanism",
        "create_write_fsync_checked_close_rename_unlink",
        dict(
            file_count=128,
            workers=1,
            bytes_per_file=4096,
            logical_write_bytes=128 * 4096,
            block_bytes=4096,
            steps=[
                "create",
                "write",
                "file_fsync",
                "checked_close",
                "rename",
                "unlink",
                "directory_fsync",
            ],
            oracle_order=[
                "verify full bytes at renamed name before unlink",
                "verify old and renamed names absent after unlink",
                "verify unrelated sentinels unchanged",
            ],
        ),
        sync="file_fsync_before_close_then_final_directory_fsync",
        oracle="rename preserves full bytes and exact names; unlinked names absent; unrelated sentinels unchanged",
    )

    for index, (numerator, workers) in enumerate([(1, 1), (1, 4), (5, 1), (5, 4)], 1):
        add(
            f"R{index:02}",
            f"{workers} workers, {numerator}/4 RAM working set",
            "resource",
            "cyclic_pwrite_existing_upper",
            dict(
                workers=workers,
                file_count=4,
                configured_guest_memory_bytes=ram,
                working_set_ram_ratio=dict(numerator=numerator, denominator=4),
                working_set_bytes=ram * numerator // 4,
                logical_write_bytes=2 * ram,
                block_bytes=4096,
                buffer_bytes_total=MIB,
                buffer_bytes_per_worker=MIB // workers,
                file_assignment="four distinct files divided among workers; no shared inode",
            ),
            sync="final_file_fsync_checked_close_and_directory_fsync",
        )

    scenarios = [
        (
            "S01",
            "repository inspect and search",
            ["enumerate", "inspect_metadata", "search_contents", "validate_search_results"],
        ),
        (
            "S02",
            "incremental edit, build and test",
            [
                "edit_fixed_sources",
                "incremental_build",
                "run_fixed_tests",
                "validate_build_and_test_outputs",
            ],
        ),
        (
            "S03",
            "offline extract and artifact publish",
            [
                "extract_offline_fixture",
                "generate_artifact",
                "fsync_artifact",
                "atomic_rename_publish",
                "fsync_publish_directory",
                "validate_artifact",
            ],
        ),
    ]
    for id_, name, steps in scenarios:
        add(
            id_,
            name,
            "scenario",
            "complete_task_graph",
            dict(steps=steps, dataset="pending_frozen_fixture_and_toolchain", argv=None),
            lower=True,
            control=id_ == "S01",
            sync="retain_all_task_declared_sync_and_close_operations",
        )
        cases[-1].update(
            primary=True,
            metric="task_completed_ms",
            task_spec_status="not_implemented; exact fixture/argv/oracle must be frozen before execution",
        )
    return cases


def curve_families():
    """Queryable extensions; no multi-axis point or executable curve is emitted."""
    return [
        dict(
            id="block",
            anchor="M06",
            axis="block_bytes",
            values=[1024, 4096, 65536, MIB],
            fixed=["logical_write_bytes", "workers", "file_count", "synchronization"],
        ),
        dict(
            id="workers",
            anchor="R01",
            axis="workers",
            values=[1, 2, 4],
            fixed=["file_count", "logical_write_bytes", "working_set_bytes", "block_bytes"],
        ),
        dict(
            id="working_set",
            anchor="R01",
            axis="working_set_ram_ratio",
            values=["1/4", "3/4", "1", "5/4", "2"],
            fixed=[
                "configured_guest_memory_bytes",
                "workers",
                "file_count",
                "logical_write_bytes",
                "block_bytes",
            ],
        ),
        dict(
            id="fsync",
            anchor="M10",
            axis="periodic_fsync_every_ops",
            values=[0, 1, 16, 256],
            fixed=["logical_write_bytes", "workers", "file_count", "block_bytes"],
            zero="final file/directory fsync still required",
        ),
        dict(
            id="file_count",
            anchor="M09",
            axis="file_count",
            values=[16, 64, 256],
            fixed=["logical_write_bytes", "workers", "block_bytes", "synchronization"],
            derived=["bytes_per_file"],
        ),
        dict(
            id="metadata_size",
            anchor="M01",
            axis="paths",
            values=[128, 2048, 16384],
            fixed=["operation", "synchronization"],
            derived=["operations"],
            normalization="operations per second; one lookup per path, so operation count scales with namespace size",
        ),
        dict(
            id="read_working_set",
            anchor="M04",
            axis="working_set_bytes",
            values=[MIB, 4 * MIB, 16 * MIB, 64 * MIB, 256 * MIB, 512 * MIB, 768 * MIB],
            fixed=["file_count", "block_bytes", "reopen_passes", "synchronization"],
            derived=["logical_read_bytes_per_pass"],
            normalization="bytes per pass divided by elapsed time; read volume scales with working set and is not fixed",
        ),
    ]


def comparison_arms(comparison):
    require(comparison in COMPARISONS, "unknown isolated comparison")
    changed = dict(zip(COMPARISONS, (GUEST_WB, HOST_WB, DATA)))[comparison]
    baseline = dict(FIXED_ENV)
    candidate = {**baseline, changed: "1"}
    return [
        dict(id="baseline", backend="vm", requested_environment=baseline),
        dict(id="candidate", backend="vm", requested_environment=candidate),
    ]


def make_plan(
    profile="quick",
    comparison="guest-writeback",
    guest_memory_mib=512,
    seed=20261017,
    include_native_reference=False,
    layer="all",
):
    require(profile in ("quick", "curves"), "unknown profile")
    require(layer in ("all", "mechanism", "resource", "scenario"), "unknown layer")
    require(type(seed) is int and 0 <= seed < 2**64, "seed must be an unsigned 64-bit integer")
    require(type(include_native_reference) is bool, "native reference selection must be boolean")
    cases = case_catalog(guest_memory_mib)
    arms = comparison_arms(comparison)
    selected = [
        case["id"]
        for case in cases
        if case["comparison_applicability"][comparison]["eligible"]
        and (layer == "all" or case["layer"] == layer)
    ]
    order = selected.copy()
    _rng(seed, "case-order").shuffle(order)
    blocks, environments, references = [], [], []
    for case_id in order:
        for pair in range(5):
            condition_order = ["baseline", "candidate"]
            _rng(seed, f"{case_id}:pair:{pair}").shuffle(condition_order)
            blocks.append(dict(case_id=case_id, pair=pair, condition_order=condition_order))
            for condition in condition_order:
                environments.append(
                    dict(
                        environment_id=f"{case_id}/{condition}/pair-{pair:02}",
                        case_id=case_id,
                        condition=condition,
                        pair=pair,
                        fresh_environment=True,
                        resident_rounds=[1, 2, 3],
                    )
                )
            if include_native_reference:
                references.append(
                    dict(
                        environment_id=f"native-reference/{case_id}/sample-{pair:02}",
                        case_id=case_id,
                        condition="native",
                        reference_sample=pair,
                        fresh_process=True,
                        resident_rounds=[1, 2, 3],
                        reference_only=True,
                    )
                )
    return dict(
        schema=1,
        design="filesystem-workloads-v2",
        benchmark_id="B-FS-ENG",
        role="offline protocol plan",
        planner="implemented",
        executor="not_implemented",
        performance_samples=False,
        measured_environments=0,
        measured_cell_round_rows=0,
        results=None,
        profile=profile,
        comparison=comparison,
        layer=layer,
        seed=seed,
        configured_guest_memory_mib=guest_memory_mib,
        planning_scope="quick anchors only; curve expansion deferred"
        if profile == "curves"
        else "quick anchors",
        execution=dict(
            supported=False,
            command=None,
            future_entry="actual semspec bench after executor/fixtures are implemented",
        ),
        catalog=cases,
        inventory=dict(
            catalog_cells=19,
            layers=dict(mechanism=12, resource=4, scenario=3),
            selected_cells=len(selected),
            selected_cell_ids=selected,
            excluded_cell_ids=[case["id"] for case in cases if case["id"] not in selected],
        ),
        comparison_contract=dict(
            arms=arms,
            changed_environment_key={
                "guest-writeback": GUEST_WB,
                "host-writeback": HOST_WB,
                "lower-read-cache": DATA,
            }[comparison],
            guest_and_host_writeback_mutually_exclusive=True,
            dax_requested=False,
            dax_actual_verified=False,
            dax_requirement="future executor must disable DAX through its actual SDK/config path; no environment switch is invented here",
            input_ownership_requirement="authenticated Immutable lower owners in both VM arms"
            if comparison == "lower-read-cache"
            else "identical input ownership in both VM arms; no new Immutable claim",
            qualification="requested only; future executor must prove per-owner negotiated settings and actual eligible paths",
            qualification_verified=False,
        ),
        sampling=dict(
            independent_unit="one fresh case x condition x pair environment",
            paired_blocks_per_case=5,
            dependent_rounds_per_environment=3,
            warmup_environments=0,
            planned_vm_environments=len(environments),
            planned_vm_cell_round_rows=3 * len(environments),
            bootstrap_unit="five paired environment blocks separately within each case; never N15",
            bootstrap_resamples=5000,
            multiplicity="co-primary scenario family requires a predeclared correction before confirmatory claims; mechanism intervals exploratory",
            quiet_gate=False,
            interference_gate=False,
            inventory_gate=False,
            failures="retain failures and valid slow values; do not estimate incomplete case contrasts or silently refill them; complete independent cases may be reported",
        ),
        case_order=order,
        paired_blocks=blocks,
        environment_schedule=environments,
        native_reference=dict(
            enabled=include_native_reference,
            reference_only=True,
            causal_contrast=False,
            requested_environment={},
            planned_environments=len(references),
            planned_cell_round_rows=3 * len(references),
            environment_schedule=references,
            scope="independent native reference processes; never pooled into VM A/B blocks",
        ),
        metrics=dict(
            co_primary=[
                dict(case_id=id_, metric="task_completed_ms") for id_ in ("S01", "S02", "S03")
            ],
            co_primary_scope="catalog-wide declarations; only selected cases belong to this collection plan",
            selected_co_primary_case_ids=[id_ for id_ in ("S01", "S02", "S03") if id_ in selected],
            aggregate_score=None,
            ready="launch to task-ready; includes fresh Host/VM where used, excludes work and exit",
            accepted="guest API returns; bytes may remain in guest cache, not necessarily host-owned",
            completed="all declared task steps and relevant background work complete; retain the case sync contract",
            durable="explicit file/directory fsync acknowledgment only; no power-loss claim",
            clocks="Ready, accepted, task completion, explicit synchronization and exit remain distinct; no artificial auxiliary FLUSH",
            verification="full deterministic guest/host bytes, exact namespace and task outcomes; placement consistent across arms",
        ),
        cache_state=dict(
            round1="fresh guest; host backing state uncontrolled and recorded, never called cold disk",
            rounds2_3="same guest and case; resident reuse with declared mutations",
            cross_case_reuse=False,
            true_cold_backing="not_implemented",
            preparation="must disclose fixture verification, application setup and any warming; never hidden",
        ),
        diagnostics=dict(
            separate_batch=True,
            profiling_in_timing=False,
            required="source-derived owner registration/final/phase inventory and explicit actual flags; no missing-to-zero",
            measurements=[
                "protocol RPC/bytes",
                "eligible paths",
                "backing attempts",
                "CPU",
                "memory",
                "dirty/writeback",
                "backpressure",
            ],
            limits="counts are not disk I/O or time shares; inclusive spans cannot be summed",
        ),
        curve_families=[
            {
                **copy.deepcopy(family),
                "status": "deferred",
                "expansion": "one axis from anchor; no Cartesian product",
            }
            for family in curve_families()
            if family["anchor"] in selected
        ],
        unresolved=[
            "execution and fixture/toolchain/oracle bindings",
            "measured behavior and performance",
            "same-inode multi-FD concurrency is a separate correctness/targeted extension",
            "complete IOCP/guest-RAM offload not implemented or evaluated by this planner",
        ],
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("quick", "curves"), default="quick")
    parser.add_argument("--comparison", choices=COMPARISONS, required=True)
    parser.add_argument(
        "--layer", choices=("all", "mechanism", "resource", "scenario"), default="all"
    )
    parser.add_argument("--guest-memory-mib", type=int, default=512)
    parser.add_argument("--seed", type=int, default=20261017)
    parser.add_argument("--include-native-reference", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args(argv)
    try:
        plan = make_plan(
            args.profile,
            args.comparison,
            args.guest_memory_mib,
            args.seed,
            args.include_native_reference,
            args.layer,
        )
        content = (json.dumps(plan, indent=2, sort_keys=True) + "\n").encode()
        if args.output is None:
            sys.stdout.write(content.decode())
        else:
            # Exclusive creation also refuses existing/dangling final symlinks.
            # Do not resolve this path or replace an existing destination.
            with args.output.open("xb") as stream:
                stream.write(content)
    except (ValueError, OSError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
