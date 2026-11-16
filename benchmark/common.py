"""Identities and byte digests used by retained-evidence readers.

The scope strings below are exact on-disk wire identities, not selectable
measurement engines. Do not use them to label new Criterion cohorts.
"""

import hashlib
import re
from pathlib import Path

from archive import save as save

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent

METRIC_SCOPE = "direct argv process launch through normal zero exit; perf_counter_ns; hooks and semspec excluded"
READY_SCOPE = "Popen launch to receipt of exact REFERENCE_READY line; perf_counter_ns; preparation and teardown excluded"
READY_EXIT_SCOPE = "same Popen launch to exact Ready line receipt and normal process exit; perf_counter_ns; preparation excluded"
FILESYSTEM_SCOPE = "Popen launch through normal zero exit of native Python metadata/read command; perf_counter_ns; interpreter, pipes and subject validation included; fixture preparation and observer excluded"


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def artifact_summary(artifacts):
    """Project retained artifact identities without leaking machine-local paths."""
    summary = {}
    for name, sha256 in artifacts.items():
        path = Path(name)
        if (
            path.suffix in (".py", ".rs")
            or path.suffix == ".json"
            and {"benchmark", "benchmark.new"}.intersection(path.parts)
            and ".data" not in path.parts
            or path.name == "semspec"
            or re.fullmatch(r"python3(?:\.\d+)?", path.name)
        ):
            continue
        if ".data" in path.parts:
            relative = path.parts[path.parts.index(".data") + 2 :]
            label = "/".join(relative) or path.name
        else:
            label = path.name if path.is_absolute() else path.as_posix()
        if label in summary and summary[label] != sha256:
            raise ValueError("ambiguous public artifact identity: " + label)
        summary[label] = sha256
    return summary
