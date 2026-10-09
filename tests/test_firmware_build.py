from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
import platform
import subprocess
import sys
import tarfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest import mock
from unittest.mock import Mock

from tests.fixtures import workspace

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-musl"
KERNEL_VERSION = "linux-6.12.1"
LIBRARY_NAME = "libkrunfw.so.5.0.0"


def _make_firmware(tmp_path: Path, resources):
    spec = importlib.util.spec_from_file_location(
        "firmware_build_test", ROOT / "scripts" / "packaging" / "firmware.py"
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    fw = tmp_path / "fw"
    fw.mkdir()
    kernel = tmp_path / f"{KERNEL_VERSION}.tar.xz"
    kernel.write_bytes(b"synthetic kernel source archive\n")
    kernel_hash = hashlib.sha256(kernel.read_bytes()).hexdigest()
    files = {
        "Makefile": (
            f"KERNEL_VERSION = {KERNEL_VERSION}\n"
            f"KERNEL_SHA256 = {kernel_hash}\n"
            "FULL_VERSION = 5.0.0\nABI_VERSION = 5\n"
        ),
        "bin2cbundle.py": "# synthetic bundle builder\n",
        "README.md": "Synthetic firmware fixture\n",
        "IMPORT.md": "Synthetic import record\n",
        "config-libkrunfw": "CONFIG_SYNTHETIC=y\n",
        "LICENSE-GPL": "Synthetic license fixture\n",
        "patches/0001-synthetic.patch": "synthetic patch\n",
        "include/synthetic.h": "#define SYNTHETIC 1\n",
    }
    for name, contents in files.items():
        path = fw / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
    builder = tmp_path / "firmware.py"
    builder.write_bytes(Path(module.__file__).read_bytes())
    resources.enter_context(mock.patch.object(module, "__file__", str(builder)))
    resources.enter_context(mock.patch.object(module, "ROOT", tmp_path))
    resources.enter_context(mock.patch.object(module, "FW", fw))
    resources.enter_context(mock.patch.object(module.sys, "platform", "linux"))
    resources.enter_context(mock.patch.object(platform, "machine", lambda: "x86_64"))
    resources.enter_context(mock.patch.dict(os.environ))
    os.environ.pop("PVISOR_FW_BUILD_DIR", None)
    for selector in ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"):
        resources.enter_context(mock.patch.dict(os.environ))
        os.environ.pop(selector, None)

    real_tools = module.build_tools
    tools = Mock(side_effect=lambda target: (["make", "SEV=0", "TDX=0"], {"cc": "fake cc 1"}, {}))
    resources.enter_context(mock.patch.object(module, "build_tools", tools))
    real_download = module.download_kernel
    download = Mock(return_value=kernel)
    resources.enter_context(mock.patch.object(module, "download_kernel", download))
    network = Mock(side_effect=AssertionError("Unexpected network access"))
    resources.enter_context(mock.patch.object(module.urllib.request, "urlopen", network))
    resources.enter_context(
        mock.patch.object(
            module.subprocess,
            "check_output",
            Mock(side_effect=AssertionError("Unexpected tool probe")),
        )
    )

    def compile_firmware(command, *, check, env):
        assert check is True
        work = Path(command[command.index("-C") + 1])
        (work / LIBRARY_NAME).write_bytes(b"synthetic firmware binary\n")
        config = work / KERNEL_VERSION / ".config"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text("CONFIG_SYNTHETIC=y\nCONFIG_ACTUAL=y\n")
        return subprocess.CompletedProcess(command, 0)

    run = Mock(side_effect=compile_firmware)
    resources.enter_context(mock.patch.object(module.subprocess, "run", run))
    target_dir = tmp_path / "target"

    def build(**kwargs):
        return module.build_firmware(TARGET, target_dir=str(target_dir), jobs="2", **kwargs)

    return SimpleNamespace(
        module=module,
        fw=fw,
        kernel=kernel,
        builder=builder,
        target_dir=target_dir,
        tools=tools,
        real_tools=real_tools,
        download=download,
        real_download=real_download,
        network=network,
        run=run,
        compile=compile_firmware,
        build=build,
    )


class FirmwareBuildTests(unittest.TestCase):
    def test_macos_case_insensitive_build_keeps_artifacts_and_sources_after_unmount(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            resources.enter_context(mock.patch.object(firmware.module.sys, "platform", "darwin"))
            resources.enter_context(mock.patch.object(platform, "machine", return_value="arm64"))
            real_exists = Path.exists
            resources.enter_context(
                mock.patch.object(
                    Path,
                    "exists",
                    lambda path: True if path.name == "caseprobe" else real_exists(path),
                )
            )
            operations = []
            library = "libkrunfw.5.dylib"

            def run(command, *, check, **kwargs):
                assert check
                if command[0] == "hdiutil":
                    operations.append(command[1])
                else:
                    build = Path(command[command.index("-C") + 1])
                    assert ".macos-fw-" in str(build)
                    (build / library).write_bytes(b"synthetic macOS firmware")
                    config = build / KERNEL_VERSION / ".config"
                    config.parent.mkdir()
                    config.write_text("CONFIG_SYNTHETIC=y\nCONFIG_ACTUAL=y\n")
                return subprocess.CompletedProcess(command, 0)

            firmware.run.side_effect = run
            output = firmware.module.build_firmware(
                "aarch64-apple-darwin", target_dir=str(firmware.target_dir)
            )
            assert output.is_file()
            assert output.parent.parent == firmware.target_dir / "fw"
            assert operations == ["create", "attach", "detach"]
            assert not list(output.parent.parent.glob(".macos-fw-*"))
            record = json.loads(firmware.module.source_record(output))
            assert record["origin"] == "pvisor/fw"
            assert record["actual_config_sha256"] == firmware.module.sha256(
                output.parent / KERNEL_VERSION / ".config"
            )
            destination = tmp_path / "sources.tar.gz"
            firmware.module.source_archive(output, destination)
            with tarfile.open(destination) as archive:
                assert (
                    archive.extractfile("actual.config").read()
                    == (output.parent / KERNEL_VERSION / ".config").read_bytes()
                )
            assert (
                firmware.module.build_firmware(
                    "aarch64-apple-darwin", target_dir=str(firmware.target_dir), offline=True
                )
                == output
            )
            assert operations == ["create", "attach", "detach"]

    def test_macos_volume_cleanup_on_failure(self):
        for failure in ("create", "attach", "build", "detach"):
            with self.subTest(failure=failure), workspace() as (tmp_path, resources):
                firmware = _make_firmware(tmp_path, resources)
                work = tmp_path / "work"
                work.mkdir()
                real_exists = Path.exists
                resources.enter_context(
                    mock.patch.object(
                        Path,
                        "exists",
                        lambda path: True if path.name == "caseprobe" else real_exists(path),
                    )
                )
                operations = []

                def run(command, *, check):
                    assert check
                    operations.append(command[1])
                    if command[1] == failure:
                        raise subprocess.CalledProcessError(1, command)

                firmware.run.side_effect = run
                with self.assertRaises(subprocess.CalledProcessError):
                    with firmware.module.firmware_build_directory(work, True):
                        if failure == "build":
                            raise subprocess.CalledProcessError(1, ["make"])
                if failure in {"build", "detach"}:
                    assert operations == ["create", "attach", "detach"]
                else:
                    assert operations[-1] == failure
                assert bool(list(tmp_path.glob(".macos-fw-*"))) == (failure == "detach")

    def test_case_sensitive_cache_needs_no_disk_image(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            real_exists = Path.exists
            resources.enter_context(
                mock.patch.object(
                    Path,
                    "exists",
                    lambda path: False if path.name == "caseprobe" else real_exists(path),
                )
            )
            with firmware.module.firmware_build_directory(tmp_path, True) as work:
                assert work == tmp_path
            firmware.run.assert_not_called()

    def test_macos_discovers_homebrew_tools_without_changing_environment(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            prefix = tmp_path / "custom-homebrew"
            paths = {
                "brew": prefix / "bin/brew",
                "uv": prefix / "bin/uv",
                "gmake": prefix / "bin/gmake",
                "clang": prefix / "opt/llvm/bin/clang",
                "ld.lld": prefix / "opt/lld/bin/ld.lld",
                **{
                    name: prefix / "opt/llvm/bin" / name
                    for name in (
                        "llvm-ar",
                        "llvm-nm",
                        "llvm-objcopy",
                        "llvm-objdump",
                        "llvm-strip",
                        "llvm-readelf",
                    )
                },
            }
            for path in paths.values():
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch(mode=0o755)
            for formula in ("make", "gnu-sed", "gnu-tar"):
                (prefix / "opt" / formula / "libexec/gnubin").mkdir(parents=True)
            resources.enter_context(mock.patch.dict(os.environ, PATH=str(prefix / "bin")))
            original_path = os.environ["PATH"]
            probe = resources.enter_context(
                mock.patch.object(
                    firmware.module.subprocess,
                    "check_output",
                    side_effect=lambda args, **kwargs: (
                        str(prefix) if args == [str(paths["brew"]), "--prefix"] else "tool 1"
                    ),
                )
            )
            elftools = resources.enter_context(
                mock.patch.object(
                    firmware.module.importlib.util, "find_spec", return_value=object()
                )
            )
            command, identities, tool_env = firmware.real_tools("aarch64-apple-darwin")
            assert command[0] == str(paths["gmake"])
            assert f"KERNEL_LD={paths['ld.lld']}" in command
            assert f"KERNEL_CC={paths['clang']}" in command
            assert "CC=/usr/bin/cc" in command
            assert identities["KERNEL_LD"] == "tool 1"
            build_path = tool_env["PATH"]
            assert build_path.split(os.pathsep)[:2] == [
                str(prefix / "opt/llvm/bin"),
                str(prefix / "opt/lld/bin"),
            ]
            assert str(prefix / "opt/gnu-sed/libexec/gnubin") in build_path.split(os.pathsep)
            assert os.environ["PATH"] == original_path
            probe.assert_any_call(
                [str(paths["brew"]), "--prefix"], text=True, stderr=subprocess.PIPE, timeout=5
            )
            elftools.return_value = None
            command, _, tool_env = firmware.real_tools("aarch64-apple-darwin")
            assert command[:6] == [
                "uv",
                "run",
                "--no-project",
                "--with",
                "pyelftools==0.33",
                str(paths["gmake"]),
            ]
            assert "PYTHON=python" in command
            assert not any(arg.startswith("PATH=") for arg in command)
            assert tool_env["PATH"] == build_path
            assert os.environ["PATH"] == original_path

    def test_macos_uses_path_when_homebrew_is_unavailable(self):
        for brew in (None, "/fake/brew"):
            with self.subTest(brew=brew), workspace() as (tmp_path, resources):
                firmware = _make_firmware(tmp_path, resources)
                resources.enter_context(mock.patch.dict(os.environ, PATH="/manual/tools"))
                resources.enter_context(
                    mock.patch.object(
                        firmware.module.shutil,
                        "which",
                        side_effect=lambda tool, **kwargs: (
                            brew if tool == "brew" else f"/manual/tools/{tool}"
                        ),
                    )
                )

                def identify_tool(args, **kwargs):
                    if args[1] == "--prefix":
                        raise subprocess.CalledProcessError(1, args)
                    return "tool 1"

                resources.enter_context(
                    mock.patch.object(firmware.module.subprocess, "check_output", identify_tool)
                )
                resources.enter_context(
                    mock.patch.object(
                        firmware.module.importlib.util, "find_spec", return_value=object()
                    )
                )
                command, _, tool_env = firmware.real_tools("aarch64-apple-darwin")
                assert "KERNEL_LD=/manual/tools/ld.lld" in command
                assert tool_env == {"PATH": "/manual/tools"}

    def test_missing_macos_linker_reports_install_command(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            resources.enter_context(
                mock.patch.object(
                    firmware.module.shutil,
                    "which",
                    side_effect=lambda tool, **kwargs: (
                        None if tool in {"brew", "ld.lld"} else f"/manual/tools/{tool}"
                    ),
                )
            )
            with self.assertRaisesRegex(
                RuntimeError, "Missing firmware tool ld.lld;.*brew install llvm lld"
            ):
                firmware.real_tools("aarch64-apple-darwin")
            firmware.download.assert_not_called()

    def test_discovered_path_is_scoped_to_the_firmware_subprocess(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            original_path = os.environ.get("PATH")
            build_path = f"{tmp_path}/llvm/bin{os.pathsep}{original_path or os.defpath}"
            firmware.tools.side_effect = None
            firmware.tools.return_value = (
                ["uv", "run", "--no-project", "--with", "pyelftools==0.33", "make"],
                {"cc": "fake cc 1"},
                {"PATH": build_path},
            )
            firmware.build(offline=True)
            assert firmware.run.call_args.kwargs["env"]["PATH"] == build_path
            assert firmware.run.call_args.args[0][:3] == ["uv", "run", "--offline"]
            assert os.environ.get("PATH") == original_path

    def test_resolver_rejects_conflicting_selectors_before_build(self):
        for selectors in [
            ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH"),
            ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"),
            ("PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"),
            ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"),
        ]:
            with self.subTest(selectors=selectors):
                with workspace() as (tmp_path, resources):
                    firmware = _make_firmware(tmp_path, resources)
                    for selector in selectors:
                        resources.enter_context(
                            mock.patch.dict(os.environ, {selector: str("missing/input")})
                        )
                    with self.assertRaisesRegex(
                        RuntimeError, "Conflicting firmware selectors"
                    ) as error:
                        firmware.module.resolve_firmware(TARGET)
                    assert all(selector in str(error.exception) for selector in selectors)
                    firmware.run.assert_not_called()
                    firmware.tools.assert_not_called()

    def test_resolver_normalizes_explicit_library(self):
        for selector in ["PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH"]:
            for form in ["relative", "home", "symlink"]:
                with self.subTest(selector=selector, form=form):
                    with workspace() as (tmp_path, resources):
                        firmware = _make_firmware(tmp_path, resources)
                        source = tmp_path / "libkrunfw.so.5.0.0"
                        source.write_bytes(b"explicit firmware")
                        resources.callback(os.chdir, os.getcwd())
                        os.chdir(tmp_path)
                        resources.enter_context(
                            mock.patch.dict(os.environ, {"HOME": str(str(tmp_path))})
                        )
                        link = tmp_path / "firmware-link"
                        link.symlink_to(source)
                        value = {
                            "relative": source.name,
                            "home": "~/" + source.name,
                            "symlink": str(link),
                        }[form]
                        resources.enter_context(mock.patch.dict(os.environ, {selector: str(value)}))
                        resolved = firmware.module.resolve_firmware(TARGET)
                        assert resolved.path == source.resolve()
                        assert resolved.cargo_env == {"PVISOR_KRUNFW_PATH": str(source.resolve())}
                        assert json.loads(resolved.source_record)[
                            "firmware_sha256"
                        ] == firmware.module.sha256(source)
                        firmware.tools.assert_not_called()

    def test_library_directory_prefers_canonical_then_unique_versioned(self):
        for canonical in [False, True]:
            with self.subTest(canonical=canonical):
                with workspace() as (tmp_path, resources):
                    firmware = _make_firmware(tmp_path, resources)
                    versioned = tmp_path / "libkrunfw.so.5.0.0"
                    versioned.write_bytes(b"versioned firmware")
                    preferred = versioned
                    if canonical:
                        preferred = tmp_path / "libkrunfw.so.5"
                        preferred.write_bytes(b"canonical firmware")
                    resources.enter_context(
                        mock.patch.dict(os.environ, {"PVISOR_LIBKRUNFW_PATH": str(str(tmp_path))})
                    )
                    resolved = firmware.module.resolve_firmware(TARGET)
                    assert resolved.path == preferred
                    assert resolved.cargo_env == {"PVISOR_KRUNFW_PATH": str(preferred)}
                    assert json.loads(resolved.source_record)[
                        "firmware_sha256"
                    ] == firmware.module.sha256(preferred)
                    firmware.tools.assert_not_called()

    def test_resolver_rejects_missing_explicit_inputs_without_fallback(self):
        for selector in ["PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"]:
            with self.subTest(selector=selector):
                with workspace() as (tmp_path, resources):
                    firmware = _make_firmware(tmp_path, resources)
                    resources.enter_context(
                        mock.patch.dict(os.environ, {selector: str(str(tmp_path / "missing"))})
                    )
                    with self.assertRaisesRegex(RuntimeError, "does not exist"):
                        firmware.module.resolve_firmware(TARGET)
                    firmware.tools.assert_not_called()

    def test_resolver_normalizes_kernel_bundle(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            bundle = tmp_path / "bundle"
            bundle.mkdir()
            for name in ("kernel.bin", "kernel.json"):
                (bundle / name).write_bytes(name.encode())
            resources.callback(os.chdir, os.getcwd())
            os.chdir(tmp_path)
            resources.enter_context(
                mock.patch.dict(os.environ, {"PVISOR_KRUNFW_KERNEL_BUNDLE": str("bundle")})
            )
            resolved = firmware.module.resolve_firmware(TARGET)
            assert resolved.cargo_env == {"PVISOR_KRUNFW_KERNEL_BUNDLE": str(bundle)}
            assert resolved.library_name is None
            record = json.loads(resolved.source_record)
            assert record["files"] == {
                name: firmware.module.sha256(bundle / name)
                for name in ("kernel.bin", "kernel.json")
            }
            with self.assertRaisesRegex(RuntimeError, "supported only on Linux"):
                firmware.module.resolve_firmware("aarch64-apple-darwin")
            firmware.tools.assert_not_called()

    def test_resolver_ignores_empty_selectors_and_preserves_build_receipt(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            for selector in (
                "PVISOR_LIBKRUNFW_PATH",
                "PVISOR_KRUNFW_PATH",
                "PVISOR_KRUNFW_KERNEL_BUNDLE",
            ):
                resources.enter_context(mock.patch.dict(os.environ, {selector: str("")}))
            resolved = firmware.module.resolve_firmware(
                TARGET, target_dir=str(firmware.target_dir), jobs="2"
            )
            assert resolved.source_record == (resolved.path.parent / "libkrunfw.SOURCE").read_text()
            assert json.loads(resolved.source_record)["firmware_sha256"] == firmware.module.sha256(
                resolved.path
            )
            firmware.run.assert_called_once()

    def test_standalone_cli_does_not_import_wheel_staging(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            captured_stdout = io.StringIO()
            resources.enter_context(redirect_stdout(captured_stdout))
            resources.enter_context(mock.patch.dict(sys.modules, {"stage_wheel_binaries": None}))
            receipt = tmp_path / "export" / "SOURCE"
            archive_path = tmp_path / "export" / "sources.tar.gz"
            resources.enter_context(
                mock.patch.object(
                    sys,
                    "argv",
                    [
                        "firmware.py",
                        "--target-dir",
                        str(firmware.target_dir),
                        "--jobs",
                        "2",
                        "--source-output",
                        str(receipt),
                        "--source-archive",
                        str(archive_path),
                    ],
                )
            )
            firmware.module.main()
            output = Path(captured_stdout.getvalue().strip())
            assert output.is_file()
            assert receipt.read_text() == firmware.module.source_record(output)
            with tarfile.open(archive_path, "r:gz") as archive:
                with archive.extractfile("libkrunfw.SOURCE") as source:
                    assert source.read() == receipt.read_bytes()
            firmware.run.assert_called_once()

    def test_build_reuses_verified_cache(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            output = firmware.build()
            receipt = output.parent / "libkrunfw.SOURCE"
            original_record = receipt.read_bytes()

            assert firmware.build(offline=True) == output
            firmware.run.assert_called_once()
            firmware.download.assert_called_once()
            firmware.network.assert_not_called()
            assert receipt.read_bytes() == original_record
            record = json.loads(original_record)
            assert record["firmware_sha256"] == firmware.module.sha256(output)
            assert record["actual_config_sha256"] == firmware.module.sha256(
                output.parent / KERNEL_VERSION / ".config"
            )
            assert record["inputs"]["config-libkrunfw"] == firmware.module.sha256(
                firmware.fw / "config-libkrunfw"
            )

    def test_source_changes_invalidate_build_cache(self):
        for name in ["config-libkrunfw", "patches/0001-synthetic.patch"]:
            with self.subTest(name=name):
                with workspace() as (tmp_path, resources):
                    firmware = _make_firmware(tmp_path, resources)
                    first = firmware.build()
                    source = firmware.fw / name
                    source.write_text(source.read_text() + "changed input\n")

                    second = firmware.build()

                    assert second != first
                    assert firmware.run.call_count == 2
                    record = json.loads((second.parent / "libkrunfw.SOURCE").read_text())
                    assert record["inputs"][name] == firmware.module.sha256(source)
                    assert (second.parent / name).read_bytes() == source.read_bytes()
                    assert firmware.build() == second
                    assert firmware.run.call_count == 2

    def test_corrupted_artifact_is_rebuilt(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            output = firmware.build()
            original = output.read_bytes()
            output.write_bytes(b"corrupted artifact")
            stale = output.parent / "stale-build-file"
            stale.write_text("must not survive rebuilding")

            assert firmware.build() == output
            assert firmware.run.call_count == 2
            assert output.read_bytes() == original
            assert not stale.exists()
            record = json.loads((output.parent / "libkrunfw.SOURCE").read_text())
            assert record["firmware_sha256"] == firmware.module.sha256(output)

    def test_failed_build_retries_with_fresh_tree(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            failed_work = []

            def fail_build(command, *, check, env):
                work = Path(command[command.index("-C") + 1])
                failed_work.append(work)
                (work / "partially-patched").write_text("stale state")
                (work / "patches/0001-synthetic.patch").write_text("partially applied patch")
                (work / LIBRARY_NAME).write_bytes(b"incomplete binary")
                raise subprocess.CalledProcessError(2, command)

            firmware.run.side_effect = fail_build
            with self.assertRaises(subprocess.CalledProcessError):
                firmware.build()
            assert not (failed_work[0] / "libkrunfw.SOURCE").exists()

            def retry_build(command, *, check, env):
                work = Path(command[command.index("-C") + 1])
                assert work == failed_work[0]
                assert not (work / "partially-patched").exists()
                assert not (work / LIBRARY_NAME).exists()
                assert (work / "patches/0001-synthetic.patch").read_bytes() == (
                    firmware.fw / "patches/0001-synthetic.patch"
                ).read_bytes()
                return firmware.compile(command, check=check, env=env)

            firmware.run.side_effect = retry_build
            output = firmware.build()
            assert output.is_file()
            assert (output.parent / "libkrunfw.SOURCE").is_file()
            assert firmware.run.call_count == 2

    def test_offline_build_without_kernel_source_never_uses_network(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            resources.enter_context(
                mock.patch.object(firmware.module, "download_kernel", firmware.real_download)
            )

            with self.assertRaisesRegex(
                RuntimeError, "Offline firmware build needs verified kernel source"
            ):
                firmware.build(offline=True)

            firmware.network.assert_not_called()
            firmware.run.assert_not_called()
            assert not list((firmware.target_dir / "fw").glob(f"{TARGET}-*"))

    def test_download_checksum_mismatch_cleans_up(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            cache = tmp_path / "downloads"
            firmware.network.side_effect = lambda *args, **kwargs: io.BytesIO(b"wrong kernel bytes")
            info = firmware.module.metadata()

            with self.assertRaisesRegex(RuntimeError, "Kernel source checksum mismatch"):
                firmware.real_download(cache, info, offline=False)

            firmware.network.assert_called_once_with(info["KERNEL_URL"], timeout=60)
            assert not (cache / f"{KERNEL_VERSION}.tar.xz").exists()
            assert not (cache / f"{KERNEL_VERSION}.tar.download").exists()
            assert list(cache.iterdir()) == []

    def test_source_record_preserves_verified_build_provenance(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            output = firmware.build()
            assert (
                firmware.module.source_record(output)
                == (output.parent / "libkrunfw.SOURCE").read_text()
            )

    def test_source_record_reports_override_honestly(self):
        for stale_receipt in [False, True]:
            with self.subTest(stale_receipt=stale_receipt):
                with workspace() as (tmp_path, resources):
                    firmware = _make_firmware(tmp_path, resources)
                    if stale_receipt:
                        output = firmware.build()
                        output.write_bytes(b"external replacement firmware")
                    else:
                        output = tmp_path / "external.so"
                        output.write_bytes(b"external firmware")

                    record = json.loads(firmware.module.source_record(output))

                    assert record["origin"] == "explicit-firmware-input"
                    assert record["path"] == str(output)
                    assert record["firmware_sha256"] == firmware.module.sha256(output)
                    assert "build_id" not in record
                    assert "kernel_source_sha256" not in record
                    with self.assertRaisesRegex(RuntimeError, "external firmware override"):
                        firmware.module.source_archive(output, tmp_path / "override-sources.tar.gz")
                    assert not (tmp_path / "override-sources.tar.gz").exists()

    def test_source_archive_contains_exact_corresponding_sources(self):
        with workspace() as (tmp_path, resources):
            firmware = _make_firmware(tmp_path, resources)
            output = firmware.build()
            record = json.loads(firmware.module.source_record(output))
            destination = tmp_path / "export" / "sources.tar.gz"

            firmware.module.source_archive(output, destination)

            expected = {
                f"fw/{name}": (output.parent / name).read_bytes() for name in record["inputs"]
            }
            expected.update(
                {
                    f"fw/tarballs/{KERNEL_VERSION}.tar.xz": firmware.kernel.read_bytes(),
                    "actual.config": (output.parent / KERNEL_VERSION / ".config").read_bytes(),
                    "libkrunfw.SOURCE": (output.parent / "libkrunfw.SOURCE").read_bytes(),
                    "build/firmware.py": firmware.builder.read_bytes(),
                }
            )
            with tarfile.open(destination, "r:gz") as archive:
                assert set(archive.getnames()) == set(expected)
                assert len(archive.getmembers()) == len(expected)
                for name, contents in expected.items():
                    member = archive.getmember(name)
                    assert member.isfile()
                    source = archive.extractfile(member)
                    assert source is not None
                    with source:
                        assert source.read() == contents

    def test_source_archive_rejects_tampering(self):
        for changed in ["input", "kernel", "actual_config", "builder"]:
            with self.subTest(changed=changed):
                with workspace() as (tmp_path, resources):
                    firmware = _make_firmware(tmp_path, resources)
                    output = firmware.build()
                    path = {
                        "input": output.parent / "config-libkrunfw",
                        "kernel": firmware.kernel,
                        "actual_config": output.parent / KERNEL_VERSION / ".config",
                        "builder": firmware.builder,
                    }[changed]
                    path.write_bytes(path.read_bytes() + b"tampered\n")
                    destination = tmp_path / "tampered-sources.tar.gz"

                    with self.assertRaises(RuntimeError):
                        firmware.module.source_archive(output, destination)

                    assert not destination.exists()
