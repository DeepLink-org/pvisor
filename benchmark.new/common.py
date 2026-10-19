"""Prepare auditable JSON inputs for semspec; no timer or sampling loop."""

import csv
import datetime as dt
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

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


def save(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", dir=path.parent, delete=False
    ) as stream:
        temporary = Path(stream.name)
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    temporary.replace(path)


def artifact_summary(artifacts):
    """Keep input/binary identities; the local manifest owns full harness records."""
    summary = {}
    for name, sha256 in artifacts.items():
        path = Path(name)
        if (
            path.suffix in (".py", ".rs")
            or path.suffix == ".json"
            and "benchmark.new" in path.parts
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


def registered(template):
    entry = str(Path(template).resolve().relative_to(ROOT))
    with (ROOT / "registry.csv").open() as stream:
        matches = [row for row in csv.DictReader(stream) if row["entry"] == entry]
    if len(matches) != 1:
        raise ValueError("template must have exactly one benchmark registry entry: " + entry)
    return matches[0]


def create_batch(template, output=None):
    template = Path(template).resolve(strict=True)
    registered(template)
    if output:
        batch = Path(output).resolve()
        if ".data" not in batch.parts:
            raise ValueError("raw output must live under an ignored .data directory")
        batch.mkdir(parents=True, exist_ok=False)
    else:
        parent = template.parent / ".data"
        parent.mkdir(exist_ok=True)
        prefix = (
            template.stem + "-" + dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-"
        )
        batch = Path(tempfile.mkdtemp(prefix=prefix, dir=parent))
    return batch


def materialize(
    template, batch, variables=None, artifacts=(), cwd=None, helpers=(), row_config=None
):
    template, batch = Path(template).resolve(), Path(batch).resolve()
    metadata = registered(template)
    variables = dict(variables or {}) | {"REPO": REPO, "BATCH": batch}
    table = json.loads(template.read_text())
    if row_config is not None:
        table = {name: table[name] for name in row_config["rows"]}
    if metadata.get("status") == "requires-single-condition-command":
        raise ValueError("template has no prepared single-condition command")
    commands = []
    for name, row in table.items():
        if row_config is not None:
            if not isinstance(row, str) or not row.startswith("$ python3 "):
                raise ValueError("script template must contain Python row commands")
            argv = shlex.split(row[2:])
            for i, arg in enumerate(argv):
                for key, replacement in variables.items():
                    arg = arg.replace("@" + key + "@", str(replacement))
                if re.search(r"@[A-Z_]+@", arg):
                    raise ValueError("unresolved row placeholder")
                argv[i] = arg
            table[name] = "$ " + shlex.join(argv)
            commands.append(
                (name, row_config["rows"][name], row_config["runs"], row_config["warmup"])
            )
            continue
        raise ValueError("registered scripts require an explicit row configuration")
    if not commands:
        raise ValueError("template has no prepared single-condition command")
    configurations = {(runs, warmup) for _, _, runs, warmup in commands}
    if len(configurations) != 1:
        raise ValueError("a batch must use one runs/warmup configuration")
    runs, warmup = configurations.pop()
    if row_config is not None:
        startup = (
            metadata["benchmark_id"] == "B-STARTUP"
            and (
                metadata["entry"] == "startup/timing.json"
                or metadata["entry"].startswith("startup/")
                and metadata["entry"].endswith("_timing.json")
            )
            and len(row_config["rows"]) == 1
            and row_config.get("metric_scope", READY_EXIT_SCOPE) == READY_EXIT_SCOPE
        )
        filesystem = (
            metadata["benchmark_id"] == "B-FS-TOOLS"
            and metadata["entry"] == "filesystem/tools.json"
            and row_config.get("metric_scope") == FILESYSTEM_SCOPE
            and 1 <= len(row_config["rows"]) <= 2
        )
        command = (
            metadata["benchmark_id"] in ("B-PROCESS", "B-REPLAY")
            and row_config.get("metric_scope") == METRIC_SCOPE
        )
        if not (startup or filesystem or command):
            raise ValueError("only registered timing scripts have row publication support")
        executable = sys.executable
    else:
        raise ValueError("registered scripts require an explicit row configuration")
    sources = (template, ROOT / "common.py", ROOT / "update.py", *helpers)
    semspec = os.environ.get("SEMSPEC_BIN")
    inputs = {
        str(Path(path).resolve(strict=True)): digest(path)
        for path in (*artifacts, *sources, executable, *([semspec] if semspec else []))
    }
    harness = batch / "harness"
    harness.mkdir()
    for source in sources:
        source = Path(source).resolve()
        destination = harness / (source.parent.name + "-" + source.name)
        shutil.copy2(source, destination)
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=REPO, capture_output=True, text=True
    ).stdout.strip()
    manifest = dict(
        **{key: metadata[key] for key in ("benchmark_id", "role", "entry")},
        batch=batch.name,
        recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),
        template_sha256=digest(template),
        metric_scope=row_config.get("metric_scope", READY_EXIT_SCOPE)
        if row_config
        else METRIC_SCOPE,
        order="sequential independent script samples; no randomized paired improvement claim",
        runs=runs,
        warmup=warmup,
        conditions=list(dict.fromkeys(condition for _, condition, _, _ in commands)),
        rows={row: condition for row, condition, _, _ in commands},
        cwd=str(Path(cwd or template.parent).resolve()),
        source_commit=commit,
        source_commit_scope="harness worktree; binary digest does not prove source-build correspondence",
        platform=platform.platform(),
        kernel=platform.release(),
        cpu_affinity=sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None,
        artifacts=inputs,
        harness={p.name: digest(p) for p in harness.iterdir()},
        status="prepared",
    )
    if row_config is not None:
        manifest.update(
            measurement_engine="python-perf_counter_ns", python_version=platform.python_version()
        )
    save(batch / "in.json", table)
    manifest["input_sha256"] = digest(batch / "in.json")
    save(batch / "manifest.json", manifest)
    return batch / "in.json"


def materialize_commands(template, batch, variables=None, artifacts=(), cwd=None, helpers=()):
    """Freeze registered direct argv workloads and their untimed correctness hooks."""
    template, batch = Path(template), Path(batch)
    definition = template.with_suffix(".commands.json")
    config = json.loads(definition.read_text())
    if (
        type(config["runs"]) is not int
        or config["runs"] < 2
        or type(config["warmup"]) is not int
        or config["warmup"] < 0
        or set(config["commands"]) != set(json.loads(template.read_text()))
    ):
        raise ValueError("invalid command runs/warmup/row configuration")
    replacements = dict(variables or {}) | {"REPO": REPO, "BATCH": batch}

    def substitute(value):
        if isinstance(value, str):
            for key, replacement in replacements.items():
                value = value.replace("@" + key + "@", str(replacement))
            if re.search(r"@[A-Z_]+@", value):
                raise ValueError("unresolved command placeholder")
        elif isinstance(value, list):
            value = [substitute(item) for item in value]
        elif isinstance(value, dict):
            value = {key: substitute(item) for key, item in value.items()}
        return value

    commands = substitute(config["commands"])
    for command in commands.values():
        if (
            not isinstance(command["argv"], list)
            or not command["argv"]
            or any(not isinstance(arg, str) for arg in command["argv"])
            or type(command["timeout"]) not in (int, float)
            or command["timeout"] <= 0
        ):
            raise ValueError("invalid direct command configuration")
        command["cwd"] = str(Path(cwd or batch).resolve())
    save(batch / "commands.json", commands)
    return materialize(
        template,
        batch,
        variables,
        [*artifacts, batch / "commands.json"],
        cwd,
        [*helpers, definition, ROOT / "command_timing.py"],
        row_config=dict(
            rows={name: name for name in commands},
            runs=config["runs"],
            warmup=config["warmup"],
            metric_scope=METRIC_SCOPE,
        ),
    )


if __name__ == "__main__":
    import argparse

    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("template", type=Path)
    p.add_argument("--output", type=Path)
    args = p.parse_args()
    try:
        batch = create_batch(args.template, args.output)
        materialize(args.template, batch)
        print(batch)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        p.exit(1, f"{error}\n")
