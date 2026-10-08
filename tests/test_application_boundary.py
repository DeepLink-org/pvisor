"""Guard runtime/application selectors without compiling Rust or firmware."""

import ast
import importlib.util
import json
import tomllib
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]


def test_workspace_separates_runtime_engines_and_application_entry_points():
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
        "pvisor-cache",
        "pvisor-tui",
        "pvisor-replay",
    }
    overlay = tomllib.loads((ROOT / "crates/pvisor-overlayfs/Cargo.toml").read_text())
    assert overlay["dependencies"]["clap"]["optional"]
    assert overlay["bin"][0]["required-features"] == ["cli"]


@pytest.fixture
def budget():
    spec = importlib.util.spec_from_file_location(
        "check_core_budget", ROOT / "scripts/ci/check_core_budget.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_budget_selects_isolated_normal_dependency_closures(budget, monkeypatch):
    commands = []

    def output(command, **kwargs):
        commands.append(command)
        return f"{command[command.index('-p') + 1]} v0.3.0\npvisor-core v0.3.0\npvisor-core v0.3.0 (*)\n"

    monkeypatch.setattr(budget.subprocess, "check_output", output)
    for package in ("pvisor", "pvisor-cli"):
        assert budget.dependency_closure(package) == {f"{package} v0.3.0", "pvisor-core v0.3.0"}
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


@pytest.mark.parametrize(
    "dependency",
    [
        "pvisor-cli",
        "pvisor-gateway",
        "pvisor-replay",
        "pvisor-tui",
        "clap",
        "ratatui",
        "crossterm",
        "vt100",
        "unicode-width",
    ],
)
def test_runtime_rejects_application_dependencies(budget, dependency):
    with pytest.raises(AssertionError, match="runtime closure"):
        budget.check_boundaries({"pvisor v0.3.0", f"{dependency} v1.0.0"}, {"pvisor-cli v0.3.0"})


@pytest.mark.parametrize("dependency", ["pvisor-gateway", "pvisor-tui"])
def test_default_application_rejects_gateway_and_retired_tui_package(budget, dependency):
    with pytest.raises(AssertionError, match="default app closure"):
        budget.check_boundaries({"pvisor v0.3.0"}, {"pvisor-cli v0.3.0", f"{dependency} v1.0.0"})


def test_application_can_link_replay_engine_and_terminal_dependencies(budget):
    budget.check_boundaries(
        {"pvisor v0.3.0", "pvisor-core v0.3.0"},
        {
            "pvisor-cli v0.3.0",
            "pvisor v0.3.0",
            "pvisor-replay v0.3.0",
            "ratatui v0.29.0",
            "vt100 v0.15.0",
            "unicode-width v0.2.0",
        },
    )


def test_budget_counts_runtime_sources_not_application_sources(
    budget, monkeypatch, tmp_path, capsys
):
    monkeypatch.chdir(tmp_path)
    for name in ("pvisor", "pvisor-core", "pvisor-cli"):
        source = tmp_path / "crates" / name / "src"
        source.mkdir(parents=True)
        (source / "lib.rs").write_text(
            "pub struct Value;\n" if name != "pvisor-cli" else "\n" * 100
        )
    binary = tmp_path / "pvisor"
    binary.write_bytes(b"app")
    monkeypatch.setattr(
        budget, "dependency_closure", lambda package: {f"{package} v0.3.0", "pvisor-core v0.3.0"}
    )
    monkeypatch.setattr(budget.subprocess, "check_output", lambda *args, **kwargs: "rustc test")
    monkeypatch.setattr(
        "sys.argv",
        [
            "check_core_budget.py",
            str(binary),
            "--max-dependencies",
            "1",
            "--max-workspace-lines",
            "2",
            "--max-core-items",
            "1",
            "--max-bytes",
            "3",
        ],
    )
    budget.main()
    metrics = json.loads(capsys.readouterr().out)
    assert metrics["runtime_package"] == "pvisor"
    assert metrics["application_package"] == "pvisor-cli"
    assert metrics["workspace_source_lines"] == 2
    assert metrics["dependencies"] == 1
    assert metrics["binary_bytes"] == 3
    monkeypatch.setattr(
        "sys.argv", ["check_core_budget.py", str(binary), "--max-dependencies", "0"]
    )
    with pytest.raises(AssertionError, match="core dependency budget exceeded"):
        budget.main()


def test_ci_runtime_and_application_shards_prepare_vm_environment():
    workflow = (ROOT / ".github/workflows/ci.yml").read_text()
    matrix = workflow.split("  rust-test:", 1)[1].split("    steps:", 1)[0]
    entries = matrix.split("- platform:")[1:]
    for package in ("pvisor", "pvisor-cli"):
        selected = [
            entry
            for entry in entries
            if package in entry.split("packages:", 1)[1].split("prepare_pvisor:", 1)[0].split()
        ]
        assert selected, f"Missing CI coverage for {package}"
        assert all("prepare_pvisor: true" in entry for entry in selected)
    assert "pvisor-tui" not in matrix
    assert "cargo build --release --locked -p pvisor --lib" in workflow
    assert "cargo build --release --locked -p pvisor-cli --bin pvisor" in workflow


def test_documentation_json_provenance_tracks_moved_application_sources():
    tree = ast.parse((ROOT / "scripts/record-doc-json.py").read_text())
    sources = next(
        ast.literal_eval(node.value)
        for node in ast.walk(tree)
        if isinstance(node, ast.Assign)
        and any(isinstance(target, ast.Name) and target.id == "sources" for target in node.targets)
    )
    assert sources == [
        "crates/pvisor/src/runtime/bundle.rs",
        "crates/pvisor-cli/src/cli/runtime.rs",
        "crates/pvisor-cli/src/cli/checkpoint.rs",
        "crates/pvisor-cli/src/cli/product.rs",
    ]
    assert all((ROOT / source).is_file() for source in sources)
