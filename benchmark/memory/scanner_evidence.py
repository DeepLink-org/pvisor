"""Read-only contracts and paired analysis of retained scanner observations."""

import hashlib
import json
import random
import statistics
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
ARMS = ("off", "ksm", "adaptive")
PATTERNS = ("repeated", "random-shared", "random-unique")
TUNABLES = (
    "run",
    "pages_to_scan",
    "sleep_millisecs",
    "smart_scan",
    "advisor_mode",
    "merge_across_nodes",
    "use_zero_pages",
    "max_page_sharing",
)


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def save(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def require_default_ksm(state):
    expected = dict(
        run="1",
        pages_to_scan="100",
        sleep_millisecs="20",
        smart_scan="1",
        merge_across_nodes="1",
        use_zero_pages="0",
        max_page_sharing="256",
    )
    if any(state["files"].get(k) != v for k, v in expected.items()) or "[none]" not in state[
        "files"
    ].get("advisor_mode", ""):
        raise ValueError("default-parameter KSM required")


def paired_interval(differences):
    rng = random.Random(20261018)
    estimates = sorted(
        statistics.median(rng.choices(differences, k=len(differences))) for _ in range(5000)
    )
    return [estimates[124], estimates[4874]]


def analyze(report):
    rows = report["executions"]
    required = {
        (r, p, a) for r in range(report["samples"]) for p in PATTERNS for a in report["arms"]
    }
    identities = [(x["round"], x["pattern"], x["arm"]) for x in rows]
    if (
        not report["complete"]
        or len(identities) != len(set(identities))
        or set(identities) != required
        or any(x["status"] != "successful" for x in rows)
    ):
        raise ValueError("incomplete/failed/duplicate cohort cannot be summarized")
    summaries, comparisons = [], []
    for pattern in PATTERNS:
        for phase in ("ready", "cow25", "cow100"):
            for arm in report["arms"]:
                selected = [x for x in rows if x["arm"] == arm and x["pattern"] == pattern]
                values = [x["metrics"][phase]["pss_mib"] for x in selected]
                summaries.append(
                    dict(
                        pattern=pattern,
                        phase=phase,
                        arm=arm,
                        n=len(values),
                        median_pss_mib=statistics.median(values),
                        range_pss_mib=[min(values), max(values)],
                        samples_pss_mib=values,
                    )
                )
            if not report["preflight"]:
                for arm in report["arms"]:
                    if arm == "off":
                        continue
                    for reference in ("off", "ksm"):
                        if reference not in report["arms"] or reference == arm:
                            continue
                        pairs = []
                        for r in range(report["samples"]):
                            group = {
                                x["arm"]: x
                                for x in rows
                                if x["pattern"] == pattern and x["round"] == r
                            }
                            pairs.append(
                                group[reference]["metrics"][phase]["pss_mib"]
                                - group[arm]["metrics"][phase]["pss_mib"]
                            )
                        interval = paired_interval(pairs)
                        comparisons.append(
                            dict(
                                pattern=pattern,
                                phase=phase,
                                arm=arm,
                                reference=reference,
                                saved_mib_median=statistics.median(pairs),
                                ci95=interval,
                                detected=not (interval[0] <= 0 <= interval[1]),
                            )
                        )
    return dict(
        summary=summaries,
        comparisons=comparisons,
        caution="Inspect retained samples for separated distributions before interpreting medians; no tail estimates.",
    )
