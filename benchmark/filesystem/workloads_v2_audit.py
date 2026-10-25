#!/usr/bin/env python3
"""Read-only, post-measurement audit of B-FS-ENG V2 shallow evidence.

No subjects, VMs, builds, rootfs scans, runtime gates or replacement samples.
Each case retains its own completeness decision when another case fails.
The receipt explicitly distinguishes observed success from missing evidence.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
MIB = 1 << 20
ARMS = ("baseline", "candidate")
FATAL = re.compile(
    r"WORKLOADS_V2_ERROR:|Kernel panic|kernel BUG at|\bBUG:|\bOops:|"
    r"general protection fault|page allocation failure:|Out of memory: Killed process"
)


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def integer(value, *, positive=False):
    return type(value) is int and value >= (1 if positive else 0)


def number(value):
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def read_json(path):
    return json.loads(Path(path).read_text())


def checked_path(root, relative):
    relative = Path(relative)
    require(
        not relative.is_absolute() and ".." not in relative.parts,
        f"unsafe evidence relative path: {relative}",
    )
    return root / relative


def budget(state, declared):
    require(isinstance(state, dict), "missing actual resource state")
    expected = {
        "affinity": declared["cpus"],
        "cpu.max": declared["quota"],
        "memory.max": str(declared["memory_bytes"]),
        "memory.swap.max": str(declared["swap_bytes"]),
    }
    require(
        all(state.get(key) == value for key, value in expected.items()),
        "actual resource budget differs from declaration",
    )


def scenario_contract(row, case):
    """Frozen tool internals have no invented syscall counts; explicit I/O only."""
    expected = {
        "S01": dict(
            metadata_ops=1024,
            read_ops=1024,
            bytes_read=4 * MIB,
            write_ops=0,
            bytes_written=0,
            periodic_fsyncs=0,
            final_fsyncs=0,
        ),
        "S02": dict(
            write_ops=1,
            bytes_written=len(f"int unit000(void) {{ return {1000 + row['round']}; }}\n".encode()),
            periodic_fsyncs=0,
            final_fsyncs=4,
        ),
        "S03": dict(
            write_ops=128 + 32768,
            bytes_written=32 * MIB + 128 * 4096,
            periodic_fsyncs=0,
            final_fsyncs=1153 + 34 + 1,
        ),
    }.get(case, {})
    require(
        all(row.get(key) == value for key, value in expected.items()),
        "scenario explicit work or synchronization contract differs",
    )


def row_contract(row, case, validate_row):
    require(isinstance(row, dict), "payload row is not an object")
    require(
        row.get("id") == case and type(row.get("round")) is int and 1 <= row["round"] <= 3,
        "payload case/round identity differs",
    )
    for key in (
        "setup_ns",
        "work_ns",
        "final_sync_ns",
        "completed_ns",
        "validation_ns",
        "foreground_read_ns",
        "bytes_written",
        "bytes_read",
        "write_ops",
        "read_ops",
        "metadata_ops",
        "periodic_fsyncs",
        "final_fsyncs",
        "checked_closes",
        "files_verified",
        "bytes_verified",
    ):
        require(integer(row.get(key)), f"missing or invalid row field {key}")
    require(row["work_ns"] > 0 and row["completed_ns"] > 0, "nonpositive task clock")
    validate_row(row, case)
    scenario_contract(row, case)
    extra_sync = {"M06": 2, "M07": 2, "M08": 2, "M09": 257, "M10": 2, "M11": 2, "M12": 1}
    if case in extra_sync:
        require(row["final_fsyncs"] == extra_sync[case], "final synchronization count differs")


def events(lines, prefix):
    return [json.loads(line[len(prefix) :]) for line in lines if line.startswith(prefix)]


def load_validator(batch):
    path = batch / "harness/workloads_v2_run.py"
    spec = importlib.util.spec_from_file_location("v2_frozen_audit_" + sha(path)[:16], path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    require(callable(getattr(module, "validate_row", None)), "frozen runner has no row validator")
    return module.validate_row


def semspec_evidence(batch, records):
    """Bind the actual export/log to the one prepared measurement command.

    Semspec script-row exports contain no exit-code field. Their existence after
    successful execution is evidence under semspec's checked-success contract;
    an explicit outer execution receipt, when supplied, is checked separately.
    """
    template = read_json(batch / "in.json")
    require(set(template) == {"filesystem-v2-shallow"}, "unexpected semspec template row")
    expression = template["filesystem-v2-shallow"]
    require(
        isinstance(expression, str) and expression.startswith("$ "), "template is not a script row"
    )
    command = expression[2:]
    require(
        command.endswith(f" {batch}/harness/workloads_v2_run.py --measure {batch}"),
        "template does not invoke the frozen batch measurement",
    )
    export = read_json(batch / "in.bench.json")
    require(
        export.get("format") == 1
        and export.get("template") == str(batch / "in.json")
        and export.get("cwd") == str(batch),
        "semspec export provenance differs",
    )
    commands, rows = export.get("commands"), export.get("rows")
    require(
        isinstance(commands, list)
        and len(commands) == 1
        and isinstance(rows, list)
        and len(rows) == 1,
        "unexpected semspec command/row inventory",
    )
    entry, row = commands[0], rows[0]
    require(
        entry.get("command") == command
        and entry.get("kind") == "script-row"
        and row.get("command") == command
        and row.get("expression") == command
        and row.get("row") == "filesystem-v2-shallow",
        "semspec command binding differs",
    )
    expected = dict(
        trials=len(records),
        failed=sum(not isinstance(r, dict) or r.get("valid") is not True for r in records),
    )
    require(
        entry.get("raw") == expected and entry.get("value") == expected,
        "semspec returned inventory differs from records",
    )
    log_path = Path(entry["log"])
    require(log_path.parent == batch / "in.bench.logs", "semspec log is outside frozen batch")
    require(read_json(log_path) == expected, "semspec log differs from export")
    explicit = batch / "semspec-execution.json"
    result = dict(
        command=command,
        export_sha256=sha(batch / "in.bench.json"),
        log_sha256=sha(log_path),
        success_inferred_from_checked_script_row_export=True,
        explicit_returncode=None,
        exit_evidence="semspec writes a script-row export only after checked zero child exit; export has no exit-code field",
    )
    if explicit.exists():
        receipt = read_json(explicit)
        require(
            type(receipt.get("returncode")) is int and receipt["returncode"] == 0,
            "outer semspec invocation did not return zero",
        )
        require(
            isinstance(receipt.get("cwd"), str) and Path(receipt["cwd"]).is_absolute(),
            "outer semspec invocation cwd missing",
        )
        argv = receipt.get("command", receipt.get("argv"))
        require(
            isinstance(argv, list)
            and all(isinstance(argument, str) for argument in argv)
            and "bench" in argv,
            "outer semspec invocation did not call bench",
        )
        invocation_cwd = Path(receipt["cwd"])

        def resolved(argument):
            path = Path(argument)
            return (path if path.is_absolute() else invocation_cwd / path).resolve()

        bench_arguments = argv[argv.index("bench") + 1 :]
        require(
            any(
                not argument.startswith("-") and resolved(argument) == batch / "in.json"
                for argument in bench_arguments
            ),
            "outer semspec invocation did not bind this in.json",
        )
        if "--cwd" in bench_arguments:
            index = bench_arguments.index("--cwd")
            require(
                bench_arguments.count("--cwd") == 1
                and index + 1 < len(bench_arguments)
                and resolved(bench_arguments[index + 1]) == batch,
                "outer semspec --cwd differs",
            )
        result.update(explicit_returncode=0, execution_receipt_sha256=sha(explicit))
    return result


def trial_evidence(batch, protocol, record, validate_row):
    case, pair, arm = record["case_id"], record["pair"], record["condition"]
    folder = batch / "trials" / f"{case}-{pair:03}-{arm}"
    require(
        read_json(folder / "result.json") == record,
        "retained trial record differs from campaign inventory",
    )
    require(
        record.get("valid") is True and record.get("host_verified") is True,
        "retained failed environment: " + str(record.get("error", "unspecified")),
    )
    require(
        type(record.get("returncode")) is int
        and record["returncode"] == 0
        and record.get("timed_out") is False,
        "environment exit/timeout failed",
    )
    expected_command = [
        str(batch / "bin/subject"),
        protocol["rootfs"],
        str(batch / "fixture-layer"),
        str(batch / "bin/libkrunfw.so"),
        str(batch / "bin/payload"),
        str(folder / "stage"),
        case,
        "3",
        str(protocol["budget"]["guest_mib"]),
    ]
    require(record.get("command") == expected_command, "trial did not use frozen case command")
    environment = record.get("environment", {})
    require(
        all(environment.get(k) == v for k, v in protocol["conditions"][arm]["env"].items()),
        "trial condition environment differs",
    )
    require(
        environment.get("PVISOR_FS_PROFILE") == "0"
        and environment.get("PVISOR_STARTUP_TIMING") == "0"
        and environment.get("PVISOR_STARTUP_PROFILE") == "0",
        "formal profiling not disabled",
    )
    budget(record.get("resource_before"), protocol["budget"])
    resource = record.get("resource", {})
    for where in ("before", "after"):
        budget(resource.get(where), protocol["budget"])
    require(
        integer(resource.get("cpu_usage_usec_delta"))
        and integer(resource.get("sampled_memory_current_peak")),
        "actual resource measurements missing",
    )
    for key in ("ready_ms", "job_ms", "exit_after_done_ms"):
        require(number(record.get(key)), f"missing or invalid environment clock {key}")
    require(
        record["ready_ms"] <= record["job_ms"] and record["exit_after_done_ms"] <= record["job_ms"],
        "Ready/exit clock outside environment",
    )
    lines = (folder / "stdout.log").read_text().splitlines()
    ready = events(lines, "WORKLOADS_V2_READY ")
    require(
        len(ready) == 1 and ready[0] == record.get("ready_record"),
        "Ready record differs from stdout",
    )
    memtotal = ready[0].get("actual_guest_memtotal_kib")
    require(integer(memtotal, positive=True), "actual guest MemTotal missing")
    done = events(lines, "WORKLOADS_V2_DONE ")
    require(
        done == [dict(id=case, rounds=3, correctness=True)],
        "missing/duplicate/different payload DONE",
    )
    rows = record.get("rows")
    require(
        isinstance(rows, list)
        and len(rows) == 3
        and all(isinstance(row, dict) for row in rows)
        and sorted(row.get("round") for row in rows) == [1, 2, 3],
        "dependent round inventory differs",
    )
    require(events(lines, "WORKLOADS_V2_RESULT ") == rows, "payload rows differ from actual stdout")
    for row in rows:
        row_contract(row, case, validate_row)
    require(
        sum(row["completed_ns"] for row in rows) / 1e6 <= record["job_ms"],
        "dependent task clocks exceed environment lifetime",
    )
    host = events(
        (folder / "host-verification.log").read_text().splitlines(), "WORKLOADS_V2_VERIFIED "
    )
    require(
        len(host) == 1
        and host[0] == record.get("host_verification")
        and host[0] == dict(id=case, final_round=3, ok=True, verified=True),
        "independent host verification receipt differs",
    )
    stderr = (folder / "stderr.log").read_text()
    require(
        "pvisor-fs-profile " not in stderr
        and "pvisor-fs-profile-start " not in stderr
        and "pvisor-fs-profile-phase " not in stderr,
        "formal trial contains diagnostic profiling",
    )
    fatal = [line for line in stderr.splitlines() if FATAL.search(line)]
    require(not fatal, "explicit guest/kernel failure in stderr: " + "\n".join(fatal[:5]))
    for key in ("oom", "oom_kill", "oom_group_kill"):
        old = dict(
            line.split() for line in resource["before"].get("memory.events", "").splitlines()
        )
        new = dict(line.split() for line in resource["after"].get("memory.events", "").splitlines())
        if key in old and key in new:
            require(int(new[key]) == int(old[key]), "explicit enclosing cgroup OOM event: " + key)
    return dict(
        case_id=case,
        pair=pair,
        condition=arm,
        round_rows=3,
        actual_guest_memtotal_kib=memtotal,
        valid=True,
        stdout_sha256=sha(folder / "stdout.log"),
        stderr_sha256=sha(folder / "stderr.log"),
        host_verification_sha256=sha(folder / "host-verification.log"),
    )


def audit(batch, *, source_root=REPO):
    batch, source_root = Path(batch).resolve(), Path(source_root).resolve()
    protocol = read_json(batch / "protocol.json")
    records = read_json(batch / "records.json")
    require(isinstance(records, list), "campaign records must be an array")
    global_errors, warnings = [], []

    def check(label, action):
        try:
            return action()
        except (ValueError, OSError, KeyError, TypeError, ImportError) as error:
            global_errors.append(dict(check=label, error=str(error)))
            return None

    def protocol_contract():
        require(
            protocol.get("benchmark_id") == "B-FS-ENG"
            and protocol.get("role") == "engineering A/B"
            and protocol.get("comparison") == "guest-writeback"
            and protocol.get("diagnostic") is False,
            "not a formal guest writeback engineering A/B batch",
        )
        require(
            protocol.get("samples") == 3 and protocol.get("resident_rounds") == 3,
            "expected three independent pairs and three dependent rounds",
        )
        require(set(protocol["conditions"]) == set(ARMS), "condition inventory differs")
        for arm in ARMS:
            env = protocol["conditions"][arm]["env"]
            require(
                env.get("PVISOR_VM_FS_WRITEBACK") == ("1" if arm == "candidate" else "0")
                and env.get("PVISOR_VM_FS_HOST_WRITEBACK") == "0"
                and env.get("PVISOR_VM_FS_LOWER_DATA_CACHE") == "0"
                and env.get("PVISOR_FS_PROFILE") == "0",
                "isolated A/B flags differ",
            )
        baseline, candidate = (protocol["conditions"][arm]["env"].copy() for arm in ARMS)
        baseline.pop("PVISOR_VM_FS_WRITEBACK")
        candidate.pop("PVISOR_VM_FS_WRITEBACK")
        require(baseline == candidate, "additional condition variables differ")
        require(
            protocol["budget"]
            == dict(
                cpus=[0, 1],
                quota="200000 100000",
                memory_bytes=2 << 30,
                swap_bytes=0,
                guest_mib=512,
                guest_vcpus=2,
            ),
            "declared budget differs",
        )
        require(
            all(
                protocol.get(k) is False
                for k in ("quiet_gate", "interference_gate", "inventory_gate")
            ),
            "unexpected measurement gate declaration",
        )

    check("protocol", protocol_contract)

    def artifacts():
        for relative, expected in protocol["artifacts"].items():
            require(
                sha(checked_path(batch, relative)) == expected,
                "frozen artifact changed: " + relative,
            )
        require(
            read_json(batch / "firmware-source/SOURCE.json")["firmware_sha256"]
            == sha(batch / "bin/libkrunfw.so"),
            "firmware SOURCE does not identify the frozen firmware",
        )
        require(
            sha(batch / "driver/src/main.rs") == sha(batch / "harness/workloads_v2_driver.rs"),
            "driver build source differs from frozen harness",
        )
        manifest = batch / "fixture-manifest.json"
        require(sha(manifest) == protocol["fixture_manifest_sha256"], "fixture manifest changed")
        fixtures = read_json(manifest)
        for relative, expected in fixtures.items():
            require(
                sha(checked_path(batch / "fixture-layer", relative)) == expected,
                "fixture changed: " + relative,
            )
        before = read_json(batch / "source-before.json")
        require(before == protocol["source_hashes"], "source-before snapshot differs from protocol")
        for relative, expected in before.items():
            require(
                sha(checked_path(source_root, relative)) == expected,
                "product source changed: " + relative,
            )
        for relative, expected in protocol.get("rootfs_tools", {}).items():
            require(
                sha(checked_path(Path(protocol["rootfs"]), relative)) == expected,
                "selected rootfs tool changed: " + relative,
            )
        return dict(
            frozen_artifacts=len(protocol["artifacts"]),
            fixture_files=len(fixtures),
            product_source_files=len(before),
            selected_rootfs_tools=len(protocol.get("rootfs_tools", {})),
            rootfs_scope="selected prepared executable identities only; no full rootfs scan",
        )

    provenance = check("artifact_fixture_source_identity", artifacts)
    for name in ("budget-before.json", "budget-after.json"):
        check(name, lambda name=name: budget(read_json(batch / name), protocol["budget"]))
    semspec = check("semspec_actual_execution", lambda: semspec_evidence(batch, records))
    if semspec and semspec.get("explicit_returncode") is None:
        warnings.append(
            "Semspec script-row export proves its checked-success path; no explicit outer exit-code receipt was supplied."
        )
    validate_row = check("load_frozen_validator", lambda: load_validator(batch))
    catalog = {case["id"]: case for case in protocol["catalog"]}
    selected = protocol["inventory"]["selected_cell_ids"]
    check(
        "selected_case_inventory",
        lambda: require(
            len(selected) == len(set(selected)) and set(selected) <= catalog.keys(),
            "selected case inventory differs",
        ),
    )
    expected = {(case, arm, pair) for case in selected for arm in ARMS for pair in range(3)}
    observed, usable, trial_receipts, defects = set(), set(), [], []
    actual_rows = 0
    for record in records:
        if not isinstance(record, dict):
            global_errors.append(
                dict(check="environment_inventory", error="non-object environment record")
            )
            continue
        key = (record.get("case_id"), record.get("condition"), record.get("pair"))
        actual_rows += len(record["rows"]) if isinstance(record.get("rows"), list) else 0
        errors = []
        if (
            type(key[0]) is not str
            or type(key[1]) is not str
            or type(key[2]) is not int
            or key not in expected
        ):
            global_errors.append(
                dict(check="environment_inventory", error=f"unexpected environment {key}")
            )
            continue
        if key in observed:
            errors.append("duplicate environment identity")
            usable.discard(key)
        observed.add(key)
        try:
            require(validate_row is not None, "frozen validator unavailable")
            receipt = trial_evidence(batch, protocol, record, validate_row)
            if not errors:
                usable.add(key)
                trial_receipts.append(receipt)
        except (ValueError, OSError, KeyError, TypeError) as error:
            errors.append(str(error))
        if errors:
            defects.append(
                dict(
                    case_id=key[0],
                    condition=key[1],
                    pair=key[2],
                    reasons=errors,
                    reported_valid=record.get("valid"),
                    retained_error=record.get("error"),
                )
            )
    for case, arm, pair in sorted(expected - observed):
        defects.append(
            dict(case_id=case, condition=arm, pair=pair, reasons=["expected environment missing"])
        )
    cases = []
    for case in selected:
        keys = {key for key in expected if key[0] == case}
        complete = keys <= usable
        cases.append(
            dict(
                case_id=case,
                layer=catalog[case]["layer"],
                expected_environments=6,
                observed_environments=len(keys & observed),
                verified_environments=len(keys & usable),
                expected_round_rows=18,
                verified_round_rows=3 * len(keys & usable),
                complete=complete,
                usable=complete and not global_errors,
                independent_pairs=3,
                dependent_rounds_per_environment=3,
            )
        )
    return dict(
        schema=1,
        benchmark_id="B-FS-ENG",
        role="post-measurement independent evidence audit",
        batch=str(batch),
        valid=not global_errors and not defects,
        status="complete_exploratory" if not global_errors and not defects else "defects_retained",
        measurement_or_build_executed=False,
        runtime_gate=False,
        identity=dict(
            protocol_sha256=sha(batch / "protocol.json"),
            records_sha256=sha(batch / "records.json"),
            auditor_sha256=sha(__file__),
        ),
        provenance=provenance,
        semspec=semspec,
        counts=dict(
            selected_cases=len(selected),
            complete_cases=sum(c["complete"] for c in cases),
            usable_cases=sum(c["usable"] for c in cases),
            expected_environments=len(expected),
            observed_environments=len(records),
            verified_environments=len(usable),
            expected_round_rows=3 * len(expected),
            observed_round_rows=actual_rows,
            verified_round_rows=3 * len(usable),
            failed_reported=sum(
                not isinstance(r, dict) or r.get("valid") is not True for r in records
            ),
            defective_or_missing_environments=len(defects),
        ),
        cases=cases,
        trials=trial_receipts,
        defects=defects,
        global_errors=global_errors,
        warnings=warnings,
        limitations=[
            "N3 exploratory; audit does not turn dependent rounds into independent samples",
            "retained host oracle receipts are checked; successful stage outputs were already removed by runner",
            "MemTotal is observed separately from configured RAM; no dirty-page or saturation claim",
            "formal profiling disabled; this audit supplies no missing protocol request counters",
        ],
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--source-root", type=Path, default=REPO)
    args = parser.parse_args(argv)
    try:
        receipt = audit(args.batch, source_root=args.source_root)
        output = args.output or args.batch / "audit.json"
        with output.open("x") as stream:
            json.dump(receipt, stream, indent=2)
            stream.write("\n")
    except (ValueError, OSError, KeyError, TypeError, ImportError) as error:
        parser.exit(2, str(error) + "\n")
    print(json.dumps(dict(valid=receipt["valid"], counts=receipt["counts"], receipt=str(output))))
    return 0 if receipt["valid"] else 1


if __name__ == "__main__":
    sys.exit(main())
