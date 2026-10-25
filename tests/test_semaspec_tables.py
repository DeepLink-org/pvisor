"""The native documentation preprocessor reads results through semaspec."""

import importlib.util
import os
import subprocess
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from tests.fixtures import workspace

ROOT = Path(__file__).resolve().parents[1]


class SemaspecTableTests(unittest.TestCase):
    def load_extension(self, resources):
        # The ordinary Python suite doesn't install the documentation generator.
        # Supply only its extension API here; the native build validates rendering.
        context = mock.Mock()
        modules = {
            "markdown": SimpleNamespace(Extension=object),
            "markdown.preprocessors": SimpleNamespace(Preprocessor=object),
            "zensical.extensions.context": SimpleNamespace(ContextPreprocessor=context),
        }
        resources.enter_context(mock.patch.dict("sys.modules", modules))
        spec = importlib.util.spec_from_file_location(
            "semaspec_tables_test", ROOT / "docs/semaspec_tables.py"
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module, context

    def test_source_path_is_relative_to_locale_and_current_markdown_is_stdin(self):
        with workspace() as (root, resources):
            extension, context = self.load_extension(resources)
            context.from_markdown.return_value = SimpleNamespace(
                config={"root_dir": str(root), "docs_dir": "src/zh"},
                page=SimpleNamespace(path="benchmarks/startup.md"),
            )
            run = resources.enter_context(mock.patch.object(extension.subprocess, "run"))
            run.return_value = subprocess.CompletedProcess([], 0, "| | ms |\n| --- | --- |\n", "")
            resources.enter_context(mock.patch.dict(os.environ, {"SEMSPEC_BIN": "/test/semaspec"}))
            processor = extension.TablePreprocessor()
            processor.md = object()
            source = ["# Results", "", '{{ table("startup.render.json") }}']
            result = processor.run(source)
            self.assertEqual(result[0], "| | ms |")
            self.assertEqual(
                run.call_args.args[0],
                [
                    "/test/semaspec",
                    "filter",
                    "--source",
                    str(root / "src/zh/benchmarks/startup.md"),
                    "--stdin",
                ],
            )
            self.assertEqual(run.call_args.kwargs["input"], "\n".join(source))

    def test_plain_pages_do_not_launch_processes_and_missing_results_fail_build(self):
        with workspace() as (root, resources):
            extension, context = self.load_extension(resources)
            context.from_markdown.return_value = SimpleNamespace(
                config={"root_dir": str(root), "docs_dir": "src/en"},
                page=SimpleNamespace(path="page.md"),
            )
            run = resources.enter_context(mock.patch.object(extension.subprocess, "run"))
            processor = extension.TablePreprocessor()
            processor.md = object()
            self.assertEqual(processor.run(["# Plain"]), ["# Plain"])
            run.assert_not_called()
            run.return_value = subprocess.CompletedProcess([], 2, "", "missing table")
            with self.assertRaisesRegex(RuntimeError, "missing table"):
                processor.run(['{{ table("missing.json") }}'])


if __name__ == "__main__":
    unittest.main()
