"""Prepare script-owned experiments from semaspec's validated Markdown inventory."""

import json
import os
import re
import shlex
import shutil
import subprocess
import sys
from pathlib import Path


def prepare_table(source, batch):
    """Read one table without executing it; freeze its source and expand argv.

    The script owns sampling. No commands are run here, and unknown placeholders
    fail before preparing external services. Frozen measurements do not depend on
    a live checkout or a second Markdown parser.
    """
    source, batch = Path(source).resolve(), Path(batch).resolve()
    binary = os.environ.get(
        "SEMSPEC_BIN", str(Path(__file__).resolve().parents[2] / "target/semaspec/debug/semaspec")
    )
    snapshot = source.read_bytes()
    result = subprocess.run(
        [binary, "filter", "--source", str(source), "--stdin", "--format", "json"],
        input=snapshot,
        check=True,
        capture_output=True,
    )
    if source.read_bytes() != snapshot:
        raise ValueError("paired specification changed while parsing")
    document = json.loads(result.stdout)
    if document["format"] != 1 or len(document["tables"]) != 1 or document["experiments"]:
        raise ValueError("a paired experiment requires one Markdown script table")
    rows = {}
    variables = {"HARNESS": batch / "harness", "BATCH": batch}
    for label, expression in document["tables"][0]["table"].items():
        if not isinstance(expression, str) or not expression.startswith("$ "):
            raise ValueError("paired experiment requires script rows")
        argv = shlex.split(expression[2:])
        for index, value in enumerate(argv):
            for key, replacement in variables.items():
                value = value.replace("@" + key + "@", str(replacement))
            if re.search(r"@[A-Z_]+@", value):
                raise ValueError("unresolved paired experiment placeholder")
            argv[index] = value
        if argv[0] == "python3":
            argv[0] = sys.executable
        rows[label] = "$ " + shlex.join(argv)
    if len(rows) != 1:
        raise ValueError("a paired experiment requires exactly one script row")
    (batch / "harness" / source.name).write_bytes(snapshot)
    shutil.copy2(__file__, batch / "harness/specification.py")
    return rows
