"""Publish a read-only benchmark run's comparison as one PR conversation comment."""

import io
import json
import math
import os
import re
import subprocess
import sys
import zipfile
import zlib
from pathlib import Path
from urllib.parse import urlencode

BACKENDS = ("linux", "rust-native", "firecracker")
MARKER = "<!-- pvisor-vm-guest-report"
MAX_REPORT = 64 * 1024


def number(value):
    if type(value) not in (float, int) or not math.isfinite(value):
        raise ValueError("invalid comparison number")
    return value


def render(report, *, startup=False):
    """Only metric slugs and formatted numbers from the artifact enter Markdown."""
    table = {}
    failures = 0
    for row in report["rows"]:
        condition = row["condition"]
        backend = next((b for b in BACKENDS if condition.startswith(b + "-")), None)
        metric = condition.removeprefix(f"{backend}-")
        if not backend or not re.fullmatch(r"[a-z0-9-]{1,64}", metric):
            raise ValueError("unknown backend or invalid metric")
        cells = table.setdefault(metric, {})
        passed = row["passed"]
        reference = backend == "firecracker"
        valid_decision = passed is None if reference else type(passed) is bool
        if backend in cells or not valid_decision:
            raise ValueError("duplicate cell or invalid gate decision")
        base, candidate, change = (
            number(row[k]) for k in ("baseline_ns_per_op", "candidate_ns_per_op", "change_pct")
        )
        if min(base, candidate) <= 0:
            raise ValueError("nonpositive median")
        cell = f"{base / 1e6:.3f} → {candidate / 1e6:.3f} ({change:+.2f}%)"
        cells[backend] = f"**{cell} ❌**" if passed is False else cell
        failures += passed is False
    if startup:
        complete = {name: set(cells) for name, cells in table.items()} == {
            "direct-running": set(BACKENDS),
            "create-running": {"linux"},
        }
    else:
        complete = len(table) == 15 and all(
            cells.keys() == set(BACKENDS) for cells in table.values()
        )
    if not complete or report["passed"] is not (failures == 0):
        raise ValueError("incomplete or inconsistent VM guest comparison")
    threshold = number(report["threshold_pct"])
    if threshold < 0:
        raise ValueError("negative threshold")
    if startup:
        return "\n".join(
            [
                f"**{'FAIL' if failures else 'PASS'}** · {failures}/3 pVisor startup cells "
                f"exceed the {threshold:g}% threshold. **Create → Running**, baseline → candidate "
                "(change), median **ms**.",
                "",
                "| Launch mode | pVisor + Linux FW | pVisor + Rust native | Firecracker (reference) |",
                "|---|---:|---:|---:|",
                "| Direct: spawn → VMM running | "
                + " | ".join(table["direct-running"][b] for b in BACKENDS)
                + " |",
                f"| Daemon: Create → sandbox Running (confirmed pool hit) | {table['create-running']['linux']} "
                "| N/A: no deferred root | N/A: not managed by pvisor-daemon |",
                "",
                "Direct measures the VMM transition; daemon measures the Running API response. "
                "Guest command execution is verified after Running, outside both clocks.",
            ]
        )
    return "\n".join(
        [
            f"**{'FAIL' if failures else 'PASS'}** · {failures}/{2 * len(table)} pVisor cells "
            f"exceed the {threshold:g}% regression threshold.",
            "",
            "Baseline → candidate (change), Criterion median **ms/workload**; lower is better. "
            "Firecracker is a reference, not a gate.",
            "",
            "| Workload | pVisor + Linux FW | pVisor + Rust native | Firecracker |",
            "|---|---:|---:|---:|",
            *(
                f"| `{metric}` | " + " | ".join(cells[b] for b in BACKENDS) + " |"
                for metric, cells in table.items()
            ),
        ]
    )


def gh(path, *args, body=None):
    cmd = ["gh", "api", path, *args]
    if body is not None:
        cmd += ["--input", "-"]
    return subprocess.run(
        cmd,
        input=json.dumps({"body": body}).encode() if body is not None else None,
        check=True,
        stdout=subprocess.PIPE,
        timeout=60,
    ).stdout


def pull_number(repo, run):
    source = run["head_repository"]
    if not source:
        return None
    if run["pull_requests"]:
        pulls = [json.loads(gh(f"{repo}/pulls/{p['number']}")) for p in run["pull_requests"]]
    else:
        # GitHub may omit the PR association for a run originating from a fork.
        query = urlencode(
            {"state": "open", "head": f"{source['owner']['login']}:{run['head_branch']}"}
        )
        pulls = json.loads(gh(f"{repo}/pulls?{query}"))
    matches = [
        p["number"]
        for p in pulls
        if p["state"] == "open"
        and p["head"]["sha"] == run["head_sha"]
        and p["head"]["repo"]
        and p["head"]["repo"]["id"] == source["id"]
        and p["head"]["ref"] == run["head_branch"]
    ]
    return matches[0] if len(matches) == 1 else None


def comparison(repo, run, kind):
    name = f"vm-{kind}-table-{run['run_attempt']}"
    artifacts = json.loads(gh(f"{repo}/actions/runs/{run['id']}/artifacts?name={name}"))[
        "artifacts"
    ]
    if not artifacts:
        if run["run_attempt"] > 1:
            jobs = json.loads(
                gh(
                    f"{repo}/actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs?per_page=100"
                )
            )["jobs"]
            if not any(j["name"] == f"VM {kind} regression gate" for j in jobs):
                return None  # Retrying an unrelated job does not supersede the VM result.
        return "**No complete comparison for this attempt.** See the benchmark run for details."
    (artifact,) = artifacts
    if artifact["name"] != name or artifact["size_in_bytes"] > MAX_REPORT or artifact["expired"]:
        raise ValueError("unexpected PR artifact")
    data = gh(f"{repo}/actions/artifacts/{artifact['id']}/zip")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        (member,) = archive.infolist()
        if (
            member.filename != "comparison.json"
            or member.file_size > MAX_REPORT
            or member.flag_bits & 1
            or member.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED)
        ):
            raise ValueError("unexpected PR artifact member")
        return render(json.loads(archive.read(member)), startup=kind == "startup")


def publish(event, kind="guest"):
    if kind not in ("guest", "startup"):
        raise ValueError("unknown report kind")
    marker = MARKER if kind == "guest" else "<!-- pvisor-vm-startup-report"
    run = event["workflow_run"]
    repo = "repos/" + event["repository"]["full_name"]
    pr = pull_number(repo, run)
    if not pr:
        return
    pages = json.loads(gh(f"{repo}/issues/{pr}/comments?per_page=100", "--paginate", "--slurp"))
    version = (run["run_number"], run["run_attempt"])
    previous = None
    for page in pages:
        for comment in page:
            match = re.match(rf"^{marker} (\d+):(\d+) -->\n", comment["body"])
            if match and comment["user"]["login"] == "github-actions[bot]":
                if tuple(map(int, match.groups())) > version:
                    return
                previous = comment["id"]
                break
    try:
        table = comparison(repo, run, kind)
    except (
        ValueError,
        TypeError,
        KeyError,
        AttributeError,
        OverflowError,
        zipfile.BadZipFile,
        zlib.error,
    ):
        table = "**Invalid comparison artifact.** No table published; inspect the benchmark run."
    if table is None:
        return
    link = f"{run['html_url']}/attempts/{run['run_attempt']}"
    body = (
        f"{marker} {version[0]}:{version[1]} -->\n## VM {kind} performance\n\n"
        f"PR head `{run['head_sha'][:12]}` · [Benchmark run]({link})\n\n{table}"
    )
    if previous is None:
        gh(f"{repo}/issues/{pr}/comments", "--method", "POST", body=body)
    else:
        gh(f"{repo}/issues/comments/{previous}", "--method", "PATCH", body=body)


if __name__ == "__main__":
    if sys.argv[1:2] in (["--guest-summary"], ["--startup-summary"]):
        print(
            render(
                json.loads(Path(sys.argv[2]).read_text()),
                startup=sys.argv[1] == "--startup-summary",
            )
        )
    else:
        publish(
            json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text()), os.environ["REPORT_KIND"]
        )
