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

ROOT = Path(__file__).resolve().parents[1]


def _make_budget():
    spec = importlib.util.spec_from_file_location(
        "check_core_budget", ROOT / "scripts/ci/check_core_budget.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ApplicationBoundaryTests(unittest.TestCase):
    def test_workspace_separates_runtime_engines_and_application_entry_points(self):
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
        assert set(workspace["members"]) == {
            f"crates/{name}"
            for name in (
                "pvisor-core",
                "pvisor",
                "pvisor-cli",
                "pvisor-daemon",
                "pvisor-shim",
                "pvisor-replay",
                "pvisor-vm",
                "pvisor-guest",
                "pvisor-overlay-core",
                "pvisor-overlayfs",
                "pvisor-overlaynet",
                "pvisor-gateway",
                "pvisor-journal",
            )
        }
        assert workspace["default-members"] == ["crates/pvisor-cli"]
        runtime = tomllib.loads((ROOT / "crates/pvisor/Cargo.toml").read_text())
        replay = tomllib.loads((ROOT / "crates/pvisor-replay/Cargo.toml").read_text())
        app = tomllib.loads((ROOT / "crates/pvisor-cli/Cargo.toml").read_text())
        assert "clap" not in runtime["dependencies"]
        assert "pvisor-cli" not in runtime["dependencies"]
        assert not (ROOT / "crates/pvisor/src/bin").exists()
        assert not {"pvisor", "pvisor-cli", "clap"} & replay["dependencies"].keys()
        assert {binary["name"] for binary in app["bin"]} == {
            "pvisor",
            "pvisor-replay",
        }
        overlay = tomllib.loads((ROOT / "crates/pvisor-overlayfs/Cargo.toml").read_text())
        assert "clap" not in overlay["dependencies"]
        assert not overlay.get("bin")
        assert not (ROOT / "crates/pvisor-overlayfs/src/main.rs").exists()

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

    def test_runtime_rejects_application_dependencies(self):
        for dependency in [
            "pvisor-cli",
            "pvisor-gateway",
            "pvisor-replay",
            "pvisor-tui",
            "clap",
            "ratatui",
            "crossterm",
            "vt100",
            "unicode-width",
        ]:
            with self.subTest(dependency=dependency):
                budget = _make_budget()
                with self.assertRaisesRegex(AssertionError, "runtime closure"):
                    budget.check_boundaries(
                        {"pvisor v0.3.0", f"{dependency} v1.0.0"}, {"pvisor-cli v0.3.0"}
                    )

    def test_default_application_rejects_gateway_and_terminal_ui_dependencies(self):
        for dependency in ["pvisor-gateway", "pvisor-tui", "ratatui", "crossterm", "vt100"]:
            with self.subTest(dependency=dependency):
                budget = _make_budget()
                with self.assertRaisesRegex(AssertionError, "default app closure"):
                    budget.check_boundaries(
                        {"pvisor v0.3.0"}, {"pvisor-cli v0.3.0", f"{dependency} v1.0.0"}
                    )

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

    def test_budget_counts_runtime_sources_not_application_sources(self):
        with ExitStack() as resources:
            budget = _make_budget()
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            captured_stdout = io.StringIO()
            resources.enter_context(redirect_stdout(captured_stdout))
            resources.callback(os.chdir, os.getcwd())
            os.chdir(tmp_path)
            for name in ("pvisor", "pvisor-core", "pvisor-cli"):
                source = tmp_path / "crates" / name / "src"
                source.mkdir(parents=True)
                (source / "lib.rs").write_text(
                    "pub struct Value;\n" if name != "pvisor-cli" else "\n" * 100
                )
            binary = tmp_path / "pvisor"
            binary.write_bytes(b"app")
            resources.enter_context(
                mock.patch.object(
                    budget,
                    "dependency_closure",
                    lambda package: {f"{package} v0.3.0", "pvisor-core v0.3.0"},
                )
            )
            resources.enter_context(
                mock.patch.object(
                    budget.subprocess, "check_output", lambda *args, **kwargs: "rustc test"
                )
            )
            resources.enter_context(
                mock.patch(
                    "sys.argv",
                    ["check_core_budget.py", str(binary)],
                )
            )
            budget.main()
            metrics = json.loads(captured_stdout.getvalue())
            assert metrics["runtime_package"] == "pvisor"
            assert metrics["application_package"] == "pvisor-cli"
            assert metrics["workspace_source_lines"] == 2
            assert metrics["dependencies"] == 1
            assert metrics["binary_bytes"] == 3
            assert metrics["core_public_declarations"] == 1

    def test_code_dependency_and_binary_growth_are_reported_without_hard_limits(self):
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
            # An obsolete, tiny baseline cannot reintroduce a default ratchet.
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
                assert metrics["dependencies"] == size + 1
                assert metrics["workspace_source_lines"] == size + 1
                assert metrics["core_public_declarations"] == size
                assert metrics["binary_bytes"] == size

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
