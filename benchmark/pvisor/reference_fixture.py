"""The same offline Python/Rust/Node project inputs as the reference matrix."""

import json
import shutil
import subprocess
from pathlib import Path
from types import SimpleNamespace

from v1.filesystem import fixture as filesystem_fixture


def prepare_fixture(work):
    work.mkdir(parents=True)
    for name in ("python", "rust/src", "node/local"):
        (work / name).mkdir(parents=True)
    (work / "python/adder.py").write_text("def add(a, b):\n    return a - b\n")
    (work / "python/test_adder.py").write_text(
        "import unittest\nfrom adder import add\nclass Adder(unittest.TestCase):\n    def test_add(self):\n        for a,b in [(1,2),(-3,5),(0,0),(999,1000)]:\n            self.assertEqual(add(a,b),a+b)\n"
    )
    (work / "rust/Cargo.toml").write_text(
        '[package]\nname="reference-project"\nversion="0.1.0"\nedition="2021"\n[workspace]\n'
    )
    (work / "rust/src/lib.rs").write_text(
        "#[test] fn grading() { for a in 0..10000 { assert_eq!(a+a,2*a); } }\n"
    )
    (work / "node/package.json").write_text(
        json.dumps(
            {
                "name": "reference",
                "version": "1.0.0",
                "scripts": {"test": "node test.js"},
                "dependencies": {f"p{i}": f"file:local/p{i}" for i in range(32)},
            }
        )
    )
    for i in range(32):
        pkg = work / f"node/local/p{i}"
        pkg.mkdir()
        (pkg / "package.json").write_text(json.dumps({"name": f"p{i}", "version": "1.0.0"}))
        (pkg / "index.js").write_text(f"module.exports = {i};\n")
    (work / "node/test.js").write_text(
        "const assert = require('assert'); for(let i=0;i<32;i++) assert.equal(require(`p${i}`),i); console.log('NODE_GRADE_PASS');\n"
    )
    for argv in (
        ["git", "init", "-q"],
        ["git", "add", "."],
        [
            "git",
            "-c",
            "user.name=Reference",
            "-c",
            "user.email=reference@invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    ):
        subprocess.run(argv, cwd=work, check=True)
    fs = filesystem_fixture(SimpleNamespace(output=work.parent, toolchain=Path("/opt/toolchain")))
    shutil.move(fs, work / "_fs")
