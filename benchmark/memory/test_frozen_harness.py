"""Frozen memory harnesses must import without repository or legacy sources."""

import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import library_compare
import pool_compare


class FrozenHarnessTests(unittest.TestCase):
    def test_each_declared_inventory_contains_its_transitive_imports(self):
        for coordinator in (pool_compare, library_compare):
            with (
                self.subTest(coordinator=coordinator.__name__),
                tempfile.TemporaryDirectory() as tmp,
            ):
                root = Path(tmp)
                harness = root / "harness"
                harness.mkdir()
                sources = coordinator.harness_paths()
                self.assertEqual(len(sources), len({path.name for path in sources}))
                for source in sources:
                    shutil.copy2(source, harness / source.name)
                code = """
import importlib, json, pathlib, sys
harness = pathlib.Path(sys.argv[1]).resolve()
sys.path.insert(0, str(harness))
importlib.import_module(sys.argv[2])
for name in json.loads(sys.argv[3]):
    importlib.import_module(name)
import memory_scale
assert pathlib.Path(memory_scale.__file__).parent == harness
modules = {name: str(pathlib.Path(module.__file__).resolve())
           for name, module in sys.modules.items()
           if getattr(module, '__file__', None)
           and pathlib.Path(module.__file__).resolve().parent == harness}
print(json.dumps(modules))
"""
                result = subprocess.run(
                    [
                        sys.executable,
                        "-I",
                        "-c",
                        code,
                        str(harness),
                        coordinator.__name__,
                        json.dumps([source.stem for source in sources]),
                    ],
                    cwd=root,
                    capture_output=True,
                    text=True,
                    check=True,
                    timeout=15,
                )
                imported = {Path(path).name for path in json.loads(result.stdout).values()}
                self.assertEqual(imported, {source.name for source in sources})
