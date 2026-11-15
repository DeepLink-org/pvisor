"""Guard runtime/application selectors without compiling Rust or firmware."""

import ast
import importlib.util
import io
import json
import os
import tempfile
import unittest
from contextlib import ExitStack, redirect_stdout
from pathlib import Path
from unittest import mock

import tomllib

from tests.fixtures import ROOT


def _make_budget():
    spec = importlib.util.spec_from_file_location(
        "check_core_budget", ROOT / "scripts/ci/check_core_budget.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ApplicationBoundaryTests(unittest.TestCase):
    def test_application_owns_cli_entry_points_without_leaking_into_engines(self):
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
        self.assertEqual(workspace["default-members"], ["crates/pvisor-cli"])
        runtime = tomllib.loads((ROOT / "crates/pvisor/Cargo.toml").read_text())
        replay = tomllib.loads((ROOT / "crates/pvisor-replay/Cargo.toml").read_text())
        app = tomllib.loads((ROOT / "crates/pvisor-cli/Cargo.toml").read_text())
        self.assertNotIn("clap", runtime["dependencies"])
        self.assertNotIn("pvisor-cli", runtime["dependencies"])
        self.assertFalse((ROOT / "crates/pvisor/src/bin").exists())
        self.assertFalse({"pvisor", "pvisor-cli", "clap"} & replay["dependencies"].keys())
        self.assertEqual({binary["name"] for binary in app["bin"]}, {"pvisor", "pvisor-replay"})
        overlay = tomllib.loads((ROOT / "crates/pvisor-overlayfs/Cargo.toml").read_text())
        self.assertNotIn("clap", overlay["dependencies"])
        self.assertFalse(overlay.get("bin"))
        self.assertFalse((ROOT / "crates/pvisor-overlayfs/src/main.rs").exists())

    def test_budget_selects_isolated_normal_dependency_closures(self):
        with ExitStack() as resources:
            budget = _make_budget()
            commands = []

            def output(command, **kwargs):
                commands.append(command)
                return f"{command[command.index('-p') + 1]} v0.3.0\npvisor-core v0.3.0\npvisor-core v0.3.0 (*)\n"

            resources.enter_context(mock.patch.object(budget.subprocess, "check_output", output))
            for package in ("pvisor", "pvisor-cli"):
                assert budget.dependency_closure(package) == {
                    f"{package} v0.3.0",
                    "pvisor-core v0.3.0",
                }
                assert commands[-1] == [
                    "cargo",
                    "tree",
                    "--locked",
                    "-p",
                    package,
                    "--edges",
                    "normal",
                    "--prefix",
                    "none",
                    "--format",
                    "{p}",
                ]

    def test_dependency_boundaries_reject_each_forbidden_edge(self):
        budget = _make_budget()
        for owner, forbidden, error in [
            (
                "runtime",
                (
                    "pvisor-cli",
                    "pvisor-gateway",
                    "pvisor-replay",
                    "pvisor-tui",
                    "clap",
                    "ratatui",
                    "crossterm",
                    "vt100",
                    "unicode-width",
                ),
                "runtime closure",
            ),
            (
                "app",
                ("pvisor-gateway", "pvisor-tui", "ratatui", "crossterm", "vt100"),
                "default app closure",
            ),
        ]:
            for dependency in forbidden:
                with self.subTest(owner=owner, dependency=dependency):
                    runtime, app = {"pvisor v0.3.0"}, {"pvisor-cli v0.3.0"}
                    (runtime if owner == "runtime" else app).add(f"{dependency} v1.0.0")
                    with self.assertRaisesRegex(AssertionError, error):
                        budget.check_boundaries(runtime, app)

    def test_application_can_link_replay_engine(self):
        budget = _make_budget()
        budget.check_boundaries(
            {"pvisor v0.3.0", "pvisor-core v0.3.0"},
            {
                "pvisor-cli v0.3.0",
                "pvisor v0.3.0",
                "pvisor-replay v0.3.0",
                "unicode-width v0.2.0",
            },
        )

    def test_budget_counts_only_runtime_sources_and_reports_growth_without_hard_limits(self):
        with ExitStack() as resources:
            budget = _make_budget()
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            captured_stdout = io.StringIO()
            resources.enter_context(redirect_stdout(captured_stdout))
            resources.callback(os.chdir, os.getcwd())
            os.chdir(tmp_path)
            (tmp_path / "crates" / "pvisor" / "src").mkdir(parents=True)
            (tmp_path / "crates" / "pvisor" / "src" / "lib.rs").write_text("pub struct Value;\n")
            (tmp_path / "crates" / "pvisor-core" / "src").mkdir(parents=True)
            app = tmp_path / "crates/pvisor-cli/src"
            app.mkdir(parents=True)
            (app / "main.rs").write_text("pub fn application() {}\n" * 2000)
            resources.enter_context(
                mock.patch.object(
                    budget,
                    "dependency_closure",
                    lambda package: (
                        {f"{package} v0.3.0", "pvisor-core v0.3.0"}
                        | {f"helper-{i} v0.3.0" for i in range(size)}
                    ),
                )
            )
            resources.enter_context(
                mock.patch.object(
                    budget.subprocess, "check_output", lambda *args, **kwargs: "rustc test"
                )
            )
            # Metric data must not impose source-size or dependency limits.
            baseline_path = tmp_path / "scripts/ci/core-budget.json"
            baseline_path.parent.mkdir(parents=True)
            baseline_path.write_text(
                json.dumps(
                    {"dependencies": 0, "workspace_source_lines": 0, "core_public_declarations": 0}
                )
            )
            binary = tmp_path / "pvisor"
            binary.write_bytes(b"")
            resources.enter_context(mock.patch("sys.argv", ["check_core_budget.py", str(binary)]))
            for size in (1, 1000):
                captured_stdout.seek(0)
                captured_stdout.truncate()
                (tmp_path / "crates" / "pvisor-core" / "src" / "lib.rs").write_text(
                    "".join(f"pub struct Core{i};\n" for i in range(size))
                )
                binary.write_bytes(b"x" * size)
                budget.main()
                metrics = json.loads(captured_stdout.getvalue())
                with self.subTest(size=size):
                    self.assertEqual(metrics["runtime_package"], "pvisor")
                    self.assertEqual(metrics["application_package"], "pvisor-cli")
                    self.assertEqual(metrics["dependencies"], size + 1)
                    self.assertEqual(metrics["workspace_source_lines"], size + 1)
                    self.assertEqual(metrics["core_public_declarations"], size)
                    self.assertEqual(metrics["binary_bytes"], size)

    def test_documentation_json_provenance_tracks_moved_application_sources(self):
        tree = ast.parse((ROOT / "scripts/record-doc-json.py").read_text())
        sources = next(
            ast.literal_eval(node.value)
            for node in ast.walk(tree)
            if isinstance(node, ast.Assign)
            and any(
                isinstance(target, ast.Name) and target.id == "sources" for target in node.targets
            )
        )
        assert sources == [
            "crates/pvisor/src/runtime/bundle.rs",
            "crates/pvisor-cli/src/cli/runtime.rs",
            "crates/pvisor-cli/src/cli/checkpoint.rs",
            "crates/pvisor-cli/src/cli/product.rs",
        ]
        assert all((ROOT / source).is_file() for source in sources)
