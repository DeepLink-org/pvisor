"""B-SWE-RESIDENT-ENG: model-free command replay, independent of native Agent continuation.

Preserves the frozen E3 runner's sequential commands, host-side model waits,
source timeout annotations and nonzero exit observations. No scheduler or policy
is installed here. Output is a new directory; admission belongs to the caller.
"""

import argparse
import hashlib
import json
import math
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from common import save
from workload_backends import build_backend

LOCALHOST_INIT = "set -eu; if [ ! -s /etc/hosts ]; then printf '127.0.0.1 localhost\\n::1 localhost ip6-localhost ip6-loopback\\n' > /etc/hosts; fi"
LOCALHOST_CHECK = "import socket; socket.getaddrinfo('localhost',0); s=socket.socket(); s.bind(('localhost',0)); s.close()"


def validate(plan):
    """Reject malformed plans before creating a sandbox (including NaN waits)."""
    if plan.get("schema") != "command-replay/v1" or not plan.get("rounds"):
        raise ValueError("expected command-replay/v1 with nonempty rounds")
    if not isinstance(plan.get("initial_check"), str) or not plan["initial_check"]:
        raise ValueError("initial_check is required: verify commit, clean tree and dependencies")
    seen = set()
    for row in plan["rounds"]:
        if type(row.get("round")) is not int or row["round"] in seen:
            raise ValueError("round identifiers must be unique integers")
        seen.add(row["round"])
        wait = row.get("wait_s")
        if wait is not None and (
            type(wait) not in (int, float) or not math.isfinite(wait) or wait < 0
        ):
            raise ValueError("wait_s must be null or a finite nonnegative duration")
        if row.get("source_outcome") not in ("completed", "timeout", "not_executed"):
            raise ValueError("unknown source_outcome")
        if row["source_outcome"] != "not_executed":
            if not isinstance(row.get("command"), str) or not row["command"]:
                raise ValueError("missing command")
            if type(row.get("source_returncode")) is not int:
                raise ValueError("missing source_returncode")
    official = plan.get("official", {})
    if not official.get("command") or not official.get("read_results_command"):
        raise ValueError("official command and read_results_command required")
    required = official.get("required_tests")
    if (
        not isinstance(required, list)
        or not required
        or any(not isinstance(n, str) or not n for n in required)
        or len(set(required)) != len(required)
    ):
        raise ValueError("required_tests must be unique nonempty names")


def classify(execution, parsed, required):
    """Parsed test statuses, not shell success, decide official correctness."""
    text = execution.get("stdout", "") + "\n" + execution.get("stderr", "")
    reasons = []
    if any(s in text for s in ("INTERNALERROR>", "ERROR collecting", "errors during collection")):
        reasons.append("evaluator_internal_or_collection_error")
    if not isinstance(parsed, dict):
        parsed = {}
        reasons.append("parser_error")
    if any(name not in parsed for name in required):
        reasons.append("missing_required_test_results")
    if any(
        parsed[n] not in ("PASSED", "FAILED", "SKIPPED", "XFAIL", "XPASS")
        for n in required
        if n in parsed
    ):
        reasons.append("test_setup_or_unknown_status")
    failed = [n for n in required if parsed.get(n) != "PASSED"]
    return dict(
        evaluator_healthy=not reasons,
        infrastructure_reasons=reasons,
        result_class="evaluator_infrastructure_error"
        if reasons
        else ("tests_failed" if failed else "tests_passed"),
        required=len(required),
        passed_count=len(required) - len(failed),
        not_passed=failed,
        passed=not reasons and not failed,
    )


def run(plan, backend, output, *, clock=time.monotonic, sleep=time.sleep):
    """One admitted task; create-to-cleanup timing includes initialization and official tests.

    Always execution deletion even after partial creation. Cleanup API success is
    distinct from a host-side physical-exit audit. No replay or teardown retries.
    """
    validate(plan)
    output = Path(output)
    output.mkdir(parents=True, exist_ok=False)
    save(output / "plan.json", plan)
    result = dict(
        schema="command-replay-result/v1",
        replay_complete=False,
        steps=[],
        failures=[],
        timings={},
        cleanup=None,
        official=None,
    )
    started = clock()

    def phase(name, fn):
        begin = clock()
        try:
            return fn()
        finally:
            result["timings"][name] = clock() - begin

    def execute(command):
        value = backend.exec(command)
        if type(value.get("return_code")) is not int:
            raise ValueError("backend did not return an integer exit code")
        if value.get("timed_out") or value.get("cancelled"):
            raise RuntimeError("local execution timeout or cancellation")
        return value

    def checked(command):
        value = execute(command)
        if value["return_code"] != 0:
            raise RuntimeError("initialization or environment preflight failed")
        return value

    try:
        phase("create_s", backend.start)

        def initialize():
            import shlex

            checked(LOCALHOST_INIT)
            checked(
                shlex.quote(plan.get("python", "python3")) + " -c " + shlex.quote(LOCALHOST_CHECK)
            )
            save(output / "initial.json", checked(plan["initial_check"]))

        phase("initialize_s", initialize)
        result["timings"].update(model_wait_s=0.0, tool_s=0.0)

        def replay():
            for row in plan["rounds"]:
                begin = clock()
                sleep(row.get("wait_s") or 0)
                result["timings"]["model_wait_s"] += clock() - begin
                if row["source_outcome"] == "not_executed":
                    result["steps"].append(dict(round=row["round"], executed=False))
                else:
                    begin = clock()
                    value = execute(row["command"])
                    elapsed = clock() - begin
                    result["timings"]["tool_s"] += elapsed
                    result["steps"].append(
                        dict(
                            round=row["round"],
                            executed=True,
                            duration_s=elapsed,
                            source_outcome=row["source_outcome"],
                            source_return_code=row["source_returncode"],
                            return_code=value["return_code"],
                            exit_matches=value["return_code"] == row["source_returncode"],
                            stdout_sha256=hashlib.sha256(
                                value.get("stdout", "").encode()
                            ).hexdigest(),
                            stderr_sha256=hashlib.sha256(
                                value.get("stderr", "").encode()
                            ).hexdigest(),
                        )
                    )
                save(output / "steps.json", result["steps"])

        phase("replay_s", replay)
        result["replay_complete"] = True

        def official():
            spec = plan["official"]
            value = execute(spec["command"])
            save(output / "official-command.json", value)
            raw = execute(spec["read_results_command"])
            save(output / "official-parser.json", raw)
            try:
                parsed = json.loads(raw["stdout"]) if raw["return_code"] == 0 else None
            except (ValueError, KeyError):
                parsed = None
            return classify(value, parsed, spec["required_tests"])

        result["official"] = phase("official_s", official)
    except BaseException as exc:
        # SDK errors can contain endpoints/tokens; retain the type, not their text.
        result["failures"].append(dict(stage="execution", type=type(exc).__name__))
        if not isinstance(exc, Exception):
            raise
    finally:
        try:
            result["cleanup"] = phase("cleanup_s", backend.stop)
        except Exception as exc:
            result["failures"].append(dict(stage="cleanup", type=type(exc).__name__))
        result["timings"]["total_s"] = clock() - started
        result["execution_completed"] = (
            result["replay_complete"]
            and not result["failures"]
            and bool(result["cleanup"] and result["cleanup"].get("api_deleted"))
        )
        result["evaluator_healthy"] = bool(
            result["official"] and result["official"]["evaluator_healthy"]
        )
        save(output / "result.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--backend", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--execute", action="store_true", help="create one sandbox; otherwise validate only"
    )
    args = parser.parse_args()
    plan_bytes = args.plan.read_bytes()
    config_bytes = args.backend.read_bytes()
    plan = json.loads(plan_bytes)
    validate(plan)
    config = json.loads(config_bytes)
    backend = build_backend(config)  # no SDK import or sandbox creation
    if not args.execute:
        print(json.dumps(dict(valid=True, started=False, rounds=len(plan["rounds"]))))
        return
    if ".data" not in args.output.resolve().parts:
        parser.error("execution output must be under an ignored .data directory")
    result = run(plan, backend, args.output)
    save(
        args.output / "inputs.json",
        dict(
            plan_sha256=hashlib.sha256(plan_bytes).hexdigest(),
            backend_sha256=hashlib.sha256(config_bytes).hexdigest(),
            backend=config["kind"],
        ),
    )
    if not result["execution_completed"] or not result["evaluator_healthy"]:
        raise SystemExit(2)


if __name__ == "__main__":
    main()
