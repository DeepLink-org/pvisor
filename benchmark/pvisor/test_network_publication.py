import copy
import json
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from publish_network import BACKENDS, MODES, expected_content, summarize, verify_outputs


def payload(mode):
    if mode == "deny":
        return dict(direct_socket_blocked=True)
    size, sha = expected_content(mode)
    result = dict(bytes=size, sha256=sha, connect_ms=1, first_byte_ms=2, elapsed_ms=3)
    return (
        dict(requests=[result.copy() for _ in range(256)], concurrency=8)
        if mode == "small"
        else result
    )


def cohort():
    conditions = [(m, b) for m in MODES for b in (("host", "vm") if m == "deny" else BACKENDS)]
    return dict(
        benchmark_id="B-NETWORK",
        prepared_inputs_unchanged=True,
        cli_arguments=dict(
            samples=30,
            warmups=3,
            network_backends=",".join(BACKENDS),
            network_modes=",".join(MODES),
            cpu_affinity="0,1",
        ),
        capabilities={"network/" + m + "/" + b: dict(state="available") for m, b in conditions},
        rows=[
            dict(
                workload=m,
                backend=b,
                trial=t,
                wall_ms=100,
                worker_ms=10,
                check=payload(m),
                correctness="passed",
                cpu_affinity=[0, 1],
            )
            for m, b in conditions
            for t in range(30)
        ],
    )


def retained_native(tmp_path):
    root = tmp_path / "trial"
    cmd = root / "commands/one"
    cmd.mkdir(parents=True)
    worker = tmp_path / "harness/v1/network_worker.py"
    worker.parent.mkdir(parents=True)
    worker.write_text("retained worker")
    (root / "workspace").mkdir()
    (root / "workspace/worker.py").write_bytes(worker.read_bytes())
    value = dict(mode="stream", worker_ms=10, check=payload("stream"), cpu_affinity=[0, 1])
    (cmd / "command.stdout").write_text(json.dumps(value) + "\n")
    (cmd / "command.stderr").write_text("")
    (cmd / "command.json").write_text(json.dumps(dict(exit_code=0, timed_out=False, wall_ms=100)))
    row = dict(
        logs=str(root),
        backend="native",
        workload="stream",
        wall_ms=100,
        worker_ms=10,
        check=value["check"],
        cpu_affinity=value["cpu_affinity"],
    )
    return dict(rows=[row]), cmd


class NetworkPublicationTests(unittest.TestCase):
    def test_network_summary_uses_batches_and_paired_conditions(self):
        rows, comparisons = summarize(cohort())
        small = next(r for r in rows if r["metric"] == "batch_median_request_ms")
        assert small["n"] == 30 and small["p50"] == 3
        assert comparisons and all(
            r["n"] == 30 and r["ci95_low_ms"] == r["ci95_high_ms"] == 0 for r in comparisons
        )

    def test_incomplete_or_incorrect_network_results_cannot_be_ranked(self):
        for mutation in [
            "missing",
            "duplicate",
            "content",
            "batch",
            "affinity",
            "deny",
            "nan",
            "inputs",
            "failure",
        ]:
            with self.subTest(mutation=mutation):
                value = cohort()
                if mutation == "missing":
                    value["rows"].pop()
                elif mutation == "duplicate":
                    value["rows"][-1] = copy.deepcopy(value["rows"][0])
                elif mutation == "content":
                    value["rows"][0]["check"]["requests"][0]["sha256"] = "wrong"
                elif mutation == "batch":
                    value["rows"][0]["check"]["requests"].pop()
                elif mutation == "affinity":
                    value["rows"][0]["cpu_affinity"] = [0, 1, 2]
                elif mutation == "deny":
                    next(r for r in value["rows"] if r["workload"] == "deny")["check"][
                        "direct_socket_blocked"
                    ] = False
                elif mutation == "nan":
                    value["rows"][0]["wall_ms"] = float("nan")
                elif mutation == "inputs":
                    value["prepared_inputs_unchanged"] = False
                else:
                    value["capabilities"]["network/small/native"]["state"] = "failed"
                with self.assertRaises(ValueError):
                    summarize(value)

    def test_network_retained_output_is_required(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            report, _ = retained_native(tmp_path)
            assert len(verify_outputs(report, tmp_path)) == 4

    def test_network_publication_rejects_replaced_output_evidence(self):
        for mutation in ["output", "worker", "command", "outside"]:
            with self.subTest(mutation=mutation):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    report, cmd = retained_native(tmp_path)
                    if mutation == "output":
                        report["rows"][0]["worker_ms"] = 20
                    elif mutation == "worker":
                        (tmp_path / "trial/workspace/worker.py").write_text("modified worker")
                    elif mutation == "command":
                        (cmd / "command.json").write_text(
                            json.dumps(dict(exit_code=1, wall_ms=100))
                        )
                    else:
                        report["rows"][0]["logs"] = str(tmp_path.parent)
                    with self.assertRaises(ValueError):
                        verify_outputs(report, tmp_path)
