"""Dedicated, offline audit and analysis for B-LAZY-STARTUP warm-prepared trials.

No commands, services or measurements are executed. ``analyze`` rejects an
incomplete/failed campaign, reads retained evidence, and writes derived summary
JSON/CSVs only after all checks pass. ``render`` returns a short Markdown report.
Bootstrap units are paired rounds; the estimands are differences and ratios of
marginal medians, including every valid slow sample.
"""

import csv
import hashlib
import json
import math
import os
import random
import re
import stat
import statistics
from pathlib import Path

GROUPS = ("oci", "turbo", "pvisor")
MARKER = b"LAZY_READY ubuntu 26.04\n"
CANARY = b"unchanged image startup input\n"
BOOTSTRAPS = 5000
BOOTSTRAP_SEED = 20261017


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    hasher = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(block)
    return hasher.hexdigest()


class Evidence:
    """Keep a checksum receipt for each file actually interpreted by the audit."""

    def __init__(self):
        self.files = {}

    def read(self, path):
        path = Path(path).resolve(strict=True)
        body = path.read_bytes()
        digest = hashlib.sha256(body).hexdigest()
        require(
            str(path) not in self.files or self.files[str(path)] == digest,
            "evidence changed during audit: " + str(path),
        )
        self.files[str(path)] = digest
        return body

    def json(self, path):
        return json.loads(self.read(path))

    def checked(self, path, digest):
        body = self.read(path)
        require(
            hashlib.sha256(body).hexdigest() == digest, "evidence digest mismatch: " + str(path)
        )
        return json.loads(body)

    def finish(self):
        for path, digest in self.files.items():
            require(sha(path) == digest, "evidence changed during audit: " + path)


def positive_ns(value):
    return type(value) is int and value > 0


def clock(record, start_key, ready_key, end_key):
    values = [record.get(key) for key in (start_key, ready_key, end_key)]
    require(
        all(positive_ns(value) for value in values), "missing positive raw monotonic timestamps"
    )
    start, ready, end = values
    require(start < ready <= end, "Ready and completion do not share an ordered clock interval")
    require(
        record.get("exit_code") == 0 and type(record.get("exit_code")) is int,
        "normal zero exit is required",
    )
    require(
        record.get("ready_ns") == ready - start and record.get("completion_ns") == end - start,
        "durations differ from retained raw timestamps",
    )
    return start, end


def artifacts(mapping):
    require(isinstance(mapping, dict) and mapping, "missing frozen artifact hashes")
    for path, digest in mapping.items():
        require(
            Path(path).is_absolute() and re.fullmatch(r"[a-f0-9]{64}", digest) is not None,
            "artifacts require absolute paths and SHA256 hex",
        )
        require(sha(path) == digest, "frozen artifact changed: " + path)


def tree_manifest(root):
    """Recompute the adapter's file/link manifest without following guest links."""
    result, pending = {}, [root]
    while pending:
        for path in sorted(pending.pop().iterdir()):
            mode = path.lstat().st_mode
            relative = str(path.relative_to(root))
            if stat.S_ISLNK(mode):
                result[relative] = {"kind": "symlink", "target": os.readlink(path)}
            elif stat.S_ISDIR(mode):
                pending.append(path)
            elif stat.S_ISREG(mode):
                result[relative] = {"kind": "regular", "sha256": sha(path)}
            else:
                raise ValueError("unsupported rootfs manifest entry: " + str(path))
    return result


def audit_pvisor(result, evidence_dir, protocol, evidence, roots):
    require(
        result.get("mode") == "prepared" and result.get("actualMode") == "virtual_machine",
        "pvisor condition must be a prepared-rootfs VM",
    )
    interval = clock(result, "clock_start_ns", "ready_observed_ns", "exit_observed_ns")
    adapter = evidence_dir.parent.parent
    prepared = evidence.checked(adapter / "prepared-input.json", result["prepared_sha"])
    require(prepared["digest"] == protocol["source_digest"], "pvisor source digest mismatch")
    require(
        prepared["workload_argv"] == protocol["workload_argv"], "pvisor prepared workload mismatch"
    )
    require(
        prepared["source"].endswith("@" + protocol["source_digest"]), "pvisor source is not pinned"
    )
    require(prepared["memory_mib"] == 2048, "pvisor guest memory differs from the 2 GiB protocol")
    evidence.checked(adapter / "source-manifest.json", protocol["source_digest"][7:])
    command = evidence.json(evidence_dir / "command.json")
    require(
        command == result["timed_argv"]
        and command[command.index("--") + 1 :] == protocol["workload_argv"],
        "pvisor retained argv differs from the common workload",
    )
    require(
        command[command.index("--rootfs") + 1] == prepared_rootfs(adapter, evidence),
        "pvisor did not use the prepared rootfs",
    )
    stdout = evidence.read(evidence_dir / "stdout.log")
    require(
        stdout.count(MARKER) == 1 and stdout.splitlines().count(MARKER.rstrip(b"\n")) == 1,
        "pvisor stdout lacks the unique exact Ready marker",
    )
    bundles = list((evidence_dir / "stage").rglob("run-bundle.json"))
    require(
        len(bundles) == 1 and Path(result["bundle"]).resolve() == bundles[0].resolve(),
        "missing, duplicate or mismatched pvisor Bundle",
    )
    bundle = evidence.json(bundles[0])
    run = bundle["run"]
    require(
        run["state"] == "completed"
        and run["exit_code"] == 0
        and run["command"] == protocol["workload_argv"]
        and run["executor"]["isolation"] == "virtual_machine"
        and bundle["safety"]["filesystem_changes_staged"] is True,
        "Bundle does not establish a successful staged VM with the common workload",
    )
    workspace = evidence_dir / "workspace"
    require(
        sorted(path.name for path in workspace.iterdir()) == ["startup-canary"]
        and evidence.read(workspace / "startup-canary") == CANARY,
        "pvisor host canary/workspace changed",
    )
    require(
        not any(
            path.is_file() or path.is_symlink()
            for path in (evidence_dir / "stage/upper").rglob("*")
        ),
        "read-only pvisor workload retained upper changes",
    )
    requests = evidence.json(evidence_dir / "requests.json")
    require(
        requests == {"rows": [], "errors": []},
        "prepared pvisor unexpectedly used the image service",
    )
    for name in ("requests", "content_bytes", "metadata_bytes", "response_bytes"):
        require(result.get(name) == 0, "prepared pvisor reports image-service activity")
    prior = roots.setdefault(str(adapter), prepared)
    require(prior == prepared, "pvisor prepared identity changed")
    return interval, prepared["cpus"]


def prepared_rootfs(adapter, evidence):
    # Read only the immutable prepared record and selected fields from protocol;
    # never copy its service authentication token into derived output.
    return evidence.json(adapter / "protocol.json")["rootfs"]


def audit_inventory(directory, evidence):
    before = evidence.json(directory / "host-before.json")
    after = evidence.json(directory / "host-after.json")
    require(
        not before["errors"] and not after["errors"] and before["boot_id"] == after["boot_id"],
        "host inventory failed or boot identity changed",
    )
    expected = {"boot_changed": False, "errors": []}
    for name in ("targets", "block_devices", "uio"):
        old, new = before[name], after[name]
        expected[name] = {
            "added": sorted(new.keys() - old.keys()),
            "removed": sorted(old.keys() - new.keys()),
            "changed": sorted(key for key in old.keys() & new.keys() if old[key] != new[key]),
        }
        require(not any(expected[name].values()), "host objects changed across container trial")
    require(not before["targets"] and not after["targets"], "TCMU targets remain across trial")
    require(
        evidence.json(directory / "host-delta.json") == expected,
        "recorded host delta differs from raw inventories",
    )


def audit_container(result, directory, protocol, evidence, provenances):
    group = result["condition"]
    require(
        result.get("action") == "trial" and result.get("scenario") == "warm-prepared",
        "container sample is not a warm-prepared timed trial",
    )
    provenance = result["provenance"]
    require(
        provenance["source_digest"] == protocol["source_digest"], "container source digest mismatch"
    )
    require(
        sha(directory / "source-manifest.json") == protocol["source_digest"][7:],
        "source manifest changed",
    )
    evidence.read(directory / "source-manifest.json")
    require(
        sha(directory / "derived-manifest.json") == provenance["derived_digest"][7:],
        "derived manifest changed",
    )
    evidence.read(directory / "derived-manifest.json")
    frozen = evidence.checked(directory / "frozen-images.json", provenance["frozen_images_sha256"])
    require(
        all(
            frozen[name] == provenance[name]
            for name in ("source_digest", "derived_digest", "source_reference", "derived_reference")
        ),
        "frozen image identities differ",
    )
    require(
        result["snapshotter"] == ("overlaybd" if group == "turbo" else "overlayfs"),
        "wrong actual snapshotter",
    )
    reference = provenance["derived_reference" if group == "turbo" else "source_reference"]
    require(result["image"] == reference, "wrong timed image reference")
    require(
        result["memory_bytes"] == 2147483648 and result["cpu_quota"] == 2,
        "container resource controls differ from protocol",
    )
    receipt = evidence.checked(directory / "prepare-receipt.json", result["prepare_receipt_sha256"])
    require(
        receipt.get("valid") is True
        and receipt.get("action") == "prepare"
        and receipt.get("budgets_verified") is True
        and receipt["condition"] == group
        and receipt["cpus"] == result["cpus"]
        and receipt["boot_id"] == result["boot_id"]
        and receipt["provenance"] == provenance,
        "container preparation identity/budget receipt differs",
    )
    for kind, names in (("configs", provenance["configs"]), ("harness", provenance["harness"])):
        for name, digest in names.items():
            require(sha(directory / name) == digest, kind + " checksum mismatch: " + name)
            evidence.read(directory / name)
    require(
        evidence.json(directory / "provenance.json") == provenance, "provenance receipt mismatch"
    )
    timing = evidence.json(directory / "trial/timing.json")
    require(
        timing.get("valid") is True and timing.get("clock") == "perf_counter_ns",
        "invalid raw container clock",
    )
    interval = clock(timing, "start_ns", "marker_ns", "end_ns")
    require(
        all(result[key] == timing[key] for key in ("ready_ns", "completion_ns")),
        "container duration mismatch",
    )
    require(
        evidence.read(directory / "trial/stdout.txt") == MARKER,
        "container stdout is not exactly the Ready marker",
    )
    command = evidence.json(directory / "trial/argv.json")
    require(
        command[:3] == ["docker", "exec", provenance["docker_container_id"]]
        and command[-len(protocol["workload_argv"]) :] == protocol["workload_argv"]
        and command[command.index("--snapshotter") + 1] == result["snapshotter"]
        and reference in command
        and "--rm" in command,
        "container raw argv differs from declared trial",
    )
    for path in sorted(directory.glob("*-command/status.json")):
        status = evidence.json(path)
        require(
            status.get("returncode") == 0 and status.get("timed_out") is False,
            "failed retained adapter command",
        )
    audit_inventory(directory, evidence)
    prior = provenances.setdefault("container", provenance)
    require(
        prior == provenance, "container runtime/configuration provenance changed between conditions"
    )
    return interval, result["cpus"]


def audit_adapter_teardown(roots, evidence):
    for directory, prepared in roots.items():
        root = Path(directory)
        teardown = evidence.json(root / "teardown.json")
        require(
            all(
                teardown.get(key) is True
                for key in ("valid", "stopped", "rootfs_unchanged", "existing_pvisor_preserved")
            )
            and teardown.get("tainted") is False
            and teardown.get("owned_namespace_survivors") == [],
            "pvisor teardown is absent, tainted or incomplete",
        )
        for artifact in prepared["artifacts"].values():
            require(sha(artifact["frozen"]) == artifact["sha256"], "frozen pvisor artifact changed")
        recorded = evidence.checked(
            root / "rootfs-manifest.json", prepared["rootfs_manifest_sha256"]
        )
        require(
            tree_manifest(Path(prepared_rootfs(root, evidence))) == recorded,
            "prepared rootfs changed",
        )


def percentile(values, quantile):
    ordered = sorted(values)
    position = (len(ordered) - 1) * quantile
    lower = math.floor(position)
    return ordered[lower] + (ordered[min(lower + 1, len(ordered) - 1)] - ordered[lower]) * (
        position - lower
    )


def distribution(values):
    xs = sorted(value / 1_000_000 for value in values)
    median = statistics.median(xs)
    result = {
        "n": len(xs),
        "median_ms": median,
        "marginal_median_ms": median,
        "mean_ms": statistics.mean(xs),
        "sd_ms": statistics.stdev(xs),
        "p95_reference_ms": percentile(xs, 0.95),
        "min_ms": xs[0],
        "max_ms": xs[-1],
        "distribution": "unsplit",
        "clusters": [],
    }
    minimum = max(5, math.ceil(len(xs) * 0.1))
    candidates = [
        (xs[index] - xs[index - 1], index) for index in range(minimum, len(xs) - minimum + 1)
    ]
    if candidates:
        gap, index = max(candidates)
        low, high = xs[:index], xs[index:]
        ordinary = statistics.median(b - a for a, b in zip(xs, xs[1:]))
        if (
            gap >= 0.2 * median
            and gap > 3 * ordinary
            and statistics.median(high) >= 1.5 * statistics.median(low)
        ):
            result.update(
                median_ms=None,
                distribution="separated-clusters",
                clusters=[
                    {
                        "n": len(cluster),
                        "fraction": len(cluster) / len(xs),
                        "median_ms": statistics.median(cluster),
                    }
                    for cluster in (low, high)
                ],
            )
    return result


def compare(left, right, metric, values, summary, seed):
    a, b = values[left][metric], values[right][metric]
    ma, mb = statistics.median(a), statistics.median(b)
    rng = random.Random(seed)
    differences, ratios = [], []
    for _ in range(BOOTSTRAPS):
        indices = rng.choices(range(30), k=30)
        x = statistics.median(a[index] for index in indices)
        y = statistics.median(b[index] for index in indices)
        differences.append((x - y) / 1_000_000)
        ratios.append(x / y)
    ci = [percentile(differences, 0.025), percentile(differences, 0.975)]
    separated = any(summary[group][metric]["clusters"] for group in (left, right))
    return {
        "left": left,
        "right": right,
        "metric": metric,
        "n_pairs": 30,
        "median_difference_ms": (ma - mb) / 1_000_000,
        "median_difference_ci95_ms": ci,
        "median_ratio": ma / mb,
        "median_ratio_ci95": [percentile(ratios, 0.025), percentile(ratios, 0.975)],
        "interpretation": "separated distributions: show cluster proportions; no single-mode ranking"
        if separated
        else (
            "no detected median difference" if ci[0] <= 0 <= ci[1] else "median difference detected"
        ),
        "single_mode_ranking": not separated,
    }


def audit_background(batch, protocol, evidence):
    rows = evidence.json(batch / "background.json")
    plan = [
        (phase, index) for phase, count in (("warmup", 3), ("sample", 30)) for index in range(count)
    ]
    require(len(rows) == len(plan), "missing per-round background observations")
    builders = {
        "cargo",
        "rustc",
        "ninja",
        "make",
        "pytest",
        "cargo-nextest",
        "nextest",
        "hyperfine",
    }
    threshold = max(1, int(protocol.get("clock_ticks", os.sysconf("SC_CLK_TCK")) * 0.2))
    for row, expected in zip(rows, plan, strict=True):
        require(
            (row.get("phase"), row.get("round")) == expected, "background round sequence mismatch"
        )
        require(
            row.get("flagged") == [],
            "foreign build/benchmark interference invalidates this entire cohort",
        )
        before, after = row["before"], row["after"]
        require(
            isinstance(before, dict) and isinstance(after, dict), "invalid raw process observations"
        )
        for pid, process in after.items():
            old = before.get(pid)
            ticks = (
                process["ticks"] - old["ticks"]
                if old and old["starttime"] == process["starttime"]
                else process["ticks"]
            )
            require(
                not (process["comm"] in builders and ticks > 0)
                and not (process["last_cpu"] in protocol["cpus"] and ticks >= threshold),
                "raw background observations contain unreported interference",
            )


def analyze(batch: Path) -> dict:
    """Audit the complete 99-trial campaign and emit reproducible derived files."""
    batch = Path(batch).resolve(strict=True)
    evidence = Evidence()
    protocol = evidence.json(batch / "protocol.json")
    records = evidence.json(batch / "records.json")
    require(
        protocol.get("scope") == "warm-prepared"
        and protocol.get("groups") == list(GROUPS)
        and protocol.get("runs") == 30
        and protocol.get("warmups") == 3,
        "unexpected campaign protocol",
    )
    require(
        re.fullmatch(r"sha256:[a-f0-9]{64}", protocol.get("source_digest", "")) is not None,
        "source must be an immutable SHA256 digest",
    )
    require(
        isinstance(protocol.get("workload_argv"), list)
        and protocol["workload_argv"]
        and all(isinstance(arg, str) and "\0" not in arg for arg in protocol["workload_argv"]),
        "invalid common workload",
    )
    artifacts(protocol["artifacts"])
    audit_background(batch, protocol, evidence)
    planned = []
    for phase, count in (("warmup", 3), ("sample", 30)):
        orders = protocol["orders"][phase]
        require(len(orders) == count, "wrong number of planned rounds")
        for index, order in enumerate(orders):
            require(
                isinstance(order, list) and sorted(order) == sorted(GROUPS),
                "round is not a complete condition permutation",
            )
            planned.extend((phase, index, condition) for condition in order)
    require(
        isinstance(records, list) and len(records) == 99,
        "exactly 9 warmup and 90 formal records are required",
    )
    values = {group: {"ready": [], "completion": []} for group in GROUPS}
    roots, provenances, paths = {}, {}, set()
    prior_end, common_cpus = None, None
    for record, expected in zip(records, planned, strict=True):
        require(
            (record.get("phase"), record.get("round"), record.get("condition")) == expected,
            "record sequence differs from frozen paired plan",
        )
        path = Path(record["result_path"])
        require(
            path.is_absolute() and str(path.resolve()) not in paths,
            "result path is relative or reused",
        )
        paths.add(str(path.resolve()))
        result = evidence.checked(path, record["result_sha256"])
        require(
            result == record["result"] and result.get("valid") is True,
            "result differs from file or trial failed",
        )
        require(result.get("workload_argv") == protocol["workload_argv"], "trial workload differs")
        require(
            positive_ns(result.get("ready_ns"))
            and positive_ns(result.get("completion_ns"))
            and result["ready_ns"] <= result["completion_ns"],
            "invalid same-call trial durations",
        )
        directory = Path(result["evidence_dir"]).resolve(strict=True)
        # The coordinator retains an independent RPC/stdout copy under requests;
        # the adapter's original result remains beside its raw evidence.
        require(
            evidence.json(directory / "result.json") == result,
            "adapter result differs from coordinator copy",
        )
        if expected[2] == "pvisor":
            interval, cpus = audit_pvisor(result, directory, protocol, evidence, roots)
        else:
            require(result.get("condition") == expected[2], "wrong container adapter condition")
            interval, cpus = audit_container(result, directory, protocol, evidence, provenances)
        require(
            prior_end is None or interval[0] >= prior_end,
            "trial clock intervals overlap or run out of order",
        )
        prior_end = interval[1]
        require(len(cpus) == 2 and len(set(cpus)) == 2, "expected two distinct workload CPUs")
        if common_cpus is None:
            common_cpus = cpus
        require(cpus == common_cpus, "workload CPU selections differ")
        if expected[0] == "sample":
            values[expected[2]]["ready"].append(result["ready_ns"])
            values[expected[2]]["completion"].append(result["completion_ns"])
    audit_adapter_teardown(roots, evidence)
    summary = {
        group: {metric: distribution(xs) for metric, xs in metrics.items()}
        for group, metrics in values.items()
    }
    comparisons = [
        compare(left, right, metric, values, summary, BOOTSTRAP_SEED + index)
        for index, (left, right, metric) in enumerate(
            (a, b, metric)
            for a, b in (("turbo", "oci"), ("pvisor", "oci"), ("pvisor", "turbo"))
            for metric in ("ready", "completion")
        )
    ]
    artifacts(protocol["artifacts"])
    evidence.finish()
    analysis = {
        "status": "passed",
        "scope": "warm-prepared",
        "source_digest": protocol["source_digest"],
        "groups": list(GROUPS),
        "runs": 30,
        "warmups": 3,
        "audited_trials": 99,
        "failed": 0,
        "excluded_slow_samples": 0,
        "workload_cpus": common_cpus,
        "summary": summary,
        "comparisons": comparisons,
        "statistics": {
            "bootstrap_replicates": BOOTSTRAPS,
            "bootstrap_seed": BOOTSTRAP_SEED,
            "unit": "paired round",
            "estimand": "difference and ratio of marginal medians",
            "split_rule": "both clusters >= max(5,10% N); largest gap >=20% median and >3x median adjacent gap; cluster medians >=1.5x",
        },
        "audit": {
            "protocol_sha256": sha(batch / "protocol.json"),
            "records_sha256": sha(batch / "records.json"),
            "evidence_sha256": evidence.files,
            "frozen_artifacts": protocol["artifacts"],
        },
        "limitations": [
            "Prepared inputs and host page cache were retained; pull/conversion/unpack/preparation costs are excluded.",
            "Container times include Docker exec; VM and container paths provide different isolation and client overhead.",
            "2 GiB guest RAM is not a whole-process or whole-node memory budget; service overhead is separate.",
            "Other tests ran on CPUs 0,1; workload affinity does not isolate shared memory, storage or host services.",
            "P95 is descriptive at N=30; separated clusters take precedence over an overall median ranking.",
        ],
    }
    (batch / "summary.json").write_text(json.dumps(analysis, indent=2, ensure_ascii=False) + "\n")
    rows = [
        {
            "condition": group,
            "metric": metric,
            **{key: value for key, value in row.items() if key != "clusters"},
            "clusters": json.dumps(row["clusters"]),
        }
        for group, metrics in summary.items()
        for metric, row in metrics.items()
    ]
    write_csv(batch / "summary.csv", rows)
    write_csv(batch / "comparisons.csv", comparisons)
    return analysis


def write_csv(path, rows):
    with path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]), lineterminator="\n")
        writer.writeheader()
        writer.writerows(
            {
                key: json.dumps(value) if isinstance(value, (list, dict)) else value
                for key, value in row.items()
            }
            for row in rows
        )


def render(batch, analysis) -> str:
    """Return a concise report; split distributions display both cluster sizes."""
    require(analysis.get("status") == "passed", "cannot render an unaudited campaign")
    lines = [
        "# 已准备镜像的新实例启动对照",
        "",
        "这些数据只衡量 warm-prepared 启动；拉取、转换、解包和服务准备不计入。",
        "每组 3 次预热、30 个配对样本，全部 99 次启动通过审计，无慢样本剔除。",
        "",
        "| 条件 | Ready 中位数／簇中位数（ms） | Completion 中位数／簇中位数（ms） |",
        "| --- | ---: | ---: |",
    ]

    def cell(row):
        if row["clusters"]:
            return "; ".join(
                f"{cluster['median_ms']:.2f} ({cluster['n']}/30)" for cluster in row["clusters"]
            )
        return f"{row['median_ms']:.2f}"

    for group in GROUPS:
        lines.append(
            f"| {group} | {cell(analysis['summary'][group]['ready'])} | {cell(analysis['summary'][group]['completion'])} |"
        )
    lines += [
        "",
        "| 比较（左减右） | 指标 | 中位数差（ms） | 配对 95% CI（ms） | 中位数比值及 95% CI |",
        "| --- | --- | ---: | --- | --- |",
    ]
    for row in analysis["comparisons"]:
        low, high = row["median_difference_ci95_ms"]
        ratio_low, ratio_high = row["median_ratio_ci95"]
        caveat = "；分离分布，不作单峰排名" if not row["single_mode_ranking"] else ""
        lines.append(
            f"| {row['left']} − {row['right']}{caveat} | {row['metric']} | {row['median_difference_ms']:.2f} | "
            f"[{low:.2f}, {high:.2f}] | {row['median_ratio']:.3f} [{ratio_low:.3f}, {ratio_high:.3f}] |"
        )
    lines += [
        "",
        "容器计时包含 Docker exec；2 GiB guest RAM 不代表整条 VM 进程树的内存上限。"
        "后台 CPU 0,1 另有测试，CPU 亲和性不能消除共享资源干扰。P95 仅供参考；结果不代表冷镜像启动、WAN、并发容量或纯镜像格式贡献。",
        "",
        "数据：summary.json、summary.csv、comparisons.csv；原始逐次证据保留于本批次目录。",
        "",
    ]
    return "\n".join(lines)
