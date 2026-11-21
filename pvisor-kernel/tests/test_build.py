"""Contracts for the directly imported source profile and ELF kernel bundling."""

import io
import json
import os
import struct
import subprocess
import sys
import tempfile
import unittest
from contextlib import ExitStack, redirect_stdout
from pathlib import Path
from unittest import mock

import tomllib

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build


class SourceTests(unittest.TestCase):
    def test_architecture_follows_the_host_or_explicit_cross_target(self):
        for machine, options, expected in (
            ("x86_64", [], "x86_64"),
            ("arm64", [], "aarch64"),
            ("aarch64", [], "aarch64"),
            ("x86_64", ["--architecture", "aarch64"], "aarch64"),
        ):
            with (
                self.subTest(machine=machine, options=options),
                mock.patch.object(build.platform, "machine", return_value=machine),
                mock.patch.object(sys, "argv", ["build.py", *options]),
                mock.patch.object(build, "build", return_value=Path("/fixture")) as run,
                redirect_stdout(io.StringIO()),
            ):
                build.main()
                self.assertEqual(run.call_args.args[0].architecture, expected)

    def test_tool_progress_goes_to_stderr_and_stdout_is_only_the_bundle_path(self):
        with tempfile.TemporaryDirectory() as temporary, ExitStack() as resources:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            (source / "provenance.json").write_text('{"toolchain":"nightly-fixture"}')
            (source / "build.py").write_text("fixture builder")
            files = [source / "provenance.json", source / "build.py"]
            nightly_bin = root / "nightly/bin"
            stable_bin = root / "stable/bin"
            resources.enter_context(
                mock.patch.dict(
                    os.environ,
                    {
                        "PATH": str(stable_bin) + os.pathsep + os.environ.get("PATH", os.defpath),
                        "CARGO": str(stable_bin / "cargo"),
                        "RUSTC": str(stable_bin / "rustc"),
                        "RUSTDOC": str(stable_bin / "rustdoc"),
                    },
                )
            )
            resources.enter_context(mock.patch.object(build, "HERE", source))
            resources.enter_context(mock.patch.object(build, "source_files", return_value=files))
            resources.enter_context(mock.patch.object(build.sys, "platform", "linux"))
            resources.enter_context(
                mock.patch.object(build.platform, "machine", return_value="x86_64")
            )
            resources.enter_context(
                mock.patch.object(
                    build.subprocess,
                    "check_output",
                    side_effect=lambda command, **kwargs: (
                        str(nightly_bin.parent)
                        if command[-2:] == ["--print", "sysroot"]
                        else "fixture rustc\n"
                    ),
                )
            )
            resources.enter_context(
                mock.patch.object(
                    build,
                    "kernel_image",
                    return_value=(bytes(8192), 0x08000000, 0x08001200),
                )
            )

            def run(command, **kwargs):
                self.assertIs(kwargs["stdout"], sys.stderr)
                if "cargo" in command or command[0].endswith("cargo-osdk"):
                    env = kwargs["env"]
                    self.assertEqual(env["PATH"].split(os.pathsep)[0], str(nightly_bin))
                    for variable, executable in (
                        ("CARGO", "cargo"),
                        ("RUSTC", "rustc"),
                        ("RUSTDOC", "rustdoc"),
                    ):
                        self.assertEqual(env[variable], str(nightly_bin / executable))
                    lock = (
                        Path(kwargs["env"]["CARGO_TARGET_DIR"])
                        / "osdk/pvisor-kernel-run-base/Cargo.lock"
                    )
                    lock.parent.mkdir(parents=True, exist_ok=True)
                    lock.write_text("fixture dependency lock")

            commands = resources.enter_context(
                mock.patch.object(build.subprocess, "run", side_effect=run)
            )
            resources.enter_context(
                mock.patch.object(
                    sys,
                    "argv",
                    [
                        "build.py",
                        "--build-dir",
                        str(root / "cache"),
                        "--prepare-toolchain",
                    ],
                )
            )
            output = io.StringIO()
            with redirect_stdout(output):
                build.main()
            directory = Path(output.getvalue().strip())
            self.assertEqual(len(output.getvalue().splitlines()), 1)
            self.assertTrue(directory.is_absolute())
            self.assertTrue((directory / "kernel.bin").is_file())
            self.assertEqual(commands.call_count, 3)

    def test_sources_are_local_and_driver_profile_is_trimmed(self):
        for name in ("kernel", "ostd", "osdk", "vdso"):
            self.assertTrue((build.HERE / name).is_dir())
        for removed in (
            "kernel/comps/drm",
            "kernel/comps/simpledrm",
            "kernel/comps/i8042",
            "kernel/core/comps/nvme",
            "kernel/core/comps/mlsdisk",
            "kernel/core/comps/virtio/src/device/block",
            "kernel/core/comps/virtio/src/device/input",
            "kernel/core/src/fs/fs_impls/ext2",
            "kernel/core/src/fs/fs_impls/exfat",
        ):
            with self.subTest(path=removed):
                self.assertFalse((build.HERE / removed).exists())
        self.assertEqual(
            tomllib.loads((build.HERE / "kernel/Cargo.toml").read_text())["package"]["name"],
            "pvisor-kernel",
        )
        self.assertEqual(
            tomllib.loads((build.HERE / "ostd/Cargo.toml").read_text())["features"]["default"],
            [],
        )

    def test_workspace_members_and_build_inputs_exist(self):
        manifest = tomllib.loads((build.HERE / "Cargo.toml").read_text())
        for member in manifest["workspace"]["members"]:
            self.assertTrue((build.HERE / member / "Cargo.toml").is_file(), member)
        files = build.source_files()
        self.assertEqual(len(files), len(set(files)))
        self.assertTrue(all(path.is_file() for path in files))
        self.assertTrue(all("target" not in path.relative_to(build.HERE).parts for path in files))
        self.assertIn(build.HERE / "kernel/core/src/fs/fs_impls/virtiofs/pvisor_ioctl.rs", files)
        self.assertIn(build.HERE / "vdso/vdso_x86_64.so", files)
        self.assertIn(build.HERE / "vdso/vdso_aarch64.so", files)
        self.assertIn(build.HERE / "ostd/src/arch/arm/boot/bsp_boot.S", files)
        self.assertIn(build.HERE / "kernel/core/src/arch/arm/signal.rs", files)
        self.assertIn(
            build.HERE / "kernel/core/comps/virtio/src/transport/mmio/bus/arch/arm.rs", files
        )
        for name in ("mod.rs", "bin.rs", "grub.rs", "grub.cfg.template", "qcow2.rs"):
            self.assertIn(build.HERE / "osdk/src/commands/build" / name, files)

    def test_build_inputs_are_not_excluded_by_repository_ignore_rules(self):
        files = [file.relative_to(build.ROOT) for file in build.source_files()]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            env = {
                **os.environ,
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_CONFIG_NOSYSTEM": "1",
            }
            subprocess.run(
                ["git", "init", "--quiet", "--template=", str(root)], env=env, check=True
            )
            ignores = {build.ROOT / ".gitignore"}
            for file in files:
                target = root / file
                target.parent.mkdir(parents=True, exist_ok=True)
                target.touch()
                for parent in (build.ROOT / file).parents:
                    if not parent.is_relative_to(build.ROOT):
                        break
                    if (parent / ".gitignore").is_file():
                        ignores.add(parent / ".gitignore")
            for ignore in ignores:
                target = root / ignore.relative_to(build.ROOT)
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(ignore.read_bytes())

            def ignored():
                return subprocess.run(
                    ["git", "check-ignore", "--no-index", "--stdin"],
                    cwd=root,
                    env=env,
                    input="".join(f"{file.as_posix()}\n" for file in files),
                    capture_output=True,
                    text=True,
                    check=False,
                )

            result = ignored()
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertEqual(result.stdout, "")
            with (root / ".gitignore").open("a") as stream:
                stream.write("\n/pvisor-kernel/osdk/src/commands/build/\n")
            result = ignored()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(
                "pvisor-kernel/osdk/src/commands/build/mod.rs", result.stdout.splitlines()
            )

    def test_provenance_and_licenses_are_retained(self):
        provenance = json.loads((build.HERE / "provenance.json").read_text())
        self.assertEqual(
            provenance["source"]["revision"], "c4b7ed575b7b32773475b194d2ad41fb6d03765b"
        )
        self.assertTrue((build.HERE / "LICENSE-MPL").is_file())
        self.assertTrue((build.HERE / ".licenserc.yaml").is_file())
        self.assertGreater(len(json.loads((build.HERE / "IMPORT-files.json").read_text())), 1000)


class ElfTests(unittest.TestCase):
    @staticmethod
    def elf() -> bytes:
        data = bytearray(0x2240)
        ident = b"\x7fELF\x02\x01\x01" + bytes(9)
        struct.pack_into(
            "<16sHHIQQQIHHHHHH",
            data,
            0,
            ident,
            2,
            62,
            1,
            0x08001200,
            64,
            0x2100,
            0,
            64,
            56,
            2,
            64,
            4,
            3,
        )
        for index, (offset, address, filesz, memsz, flags) in enumerate(
            [(0x1000, 0x08000000, 16, 4096, 4), (0x1200, 0x08001000, 1024, 4096, 5)]
        ):
            struct.pack_into(
                "<IIQQQQQQ",
                data,
                64 + index * 56,
                1,
                flags,
                offset,
                address,
                address,
                filesz,
                memsz,
                4096,
            )
        data[0x1000:0x1010] = bytes(range(16))
        data[0x1200:0x1600] = bytes([0xAB]) * 1024
        strings = b"\0__linux64_boot\0"
        data[0x2000 : 0x2000 + len(strings)] = strings
        struct.pack_into("<IBBHQQ", data, 0x2040 + 24, 1, 0x12, 0, 1, 0x08001200, 1)
        names = b"\0.symtab\0.strtab\0.shstrtab\0"
        data[0x2080 : 0x2080 + len(names)] = names
        struct.pack_into("<IIQQQQIIQQ", data, 0x2140, 1, 2, 0, 0, 0x2040, 48, 2, 1, 8, 24)
        struct.pack_into("<IIQQQQIIQQ", data, 0x2180, 9, 3, 0, 0, 0x2000, len(strings), 0, 0, 1, 0)
        struct.pack_into("<IIQQQQIIQQ", data, 0x21C0, 17, 3, 0, 0, 0x2080, len(names), 0, 0, 1, 0)
        return bytes(data)

    @classmethod
    def arm_elf(cls) -> bytes:
        data = bytearray(cls.elf())
        struct.pack_into("<H", data, 18, 183)
        struct.pack_into("<Q", data, 24, 0x80200040)
        for index in range(2):
            for field in (16, 24):
                struct.pack_into("<Q", data, 64 + index * 56 + field, 0x80200000 + index * 4096)
        struct.pack_into("<I", data, 64 + 4, 5)
        struct.pack_into("<Q", data, 64 + 32, 64)
        data[0x1000:0x1040] = struct.pack(
            "<IIQQQQQQII", 0x94000010, 0, 0x200000, 8192, 2, 0, 0, 0, 0x644D5241, 0
        )
        data[0x2000:0x2010] = b"\0_header\0" + bytes(7)
        struct.pack_into("<Q", data, 0x2040 + 24 + 8, 0x80200000)
        return bytes(data)

    def test_arm_entry_executes_the_image_header_before_the_elf_entry(self):
        with tempfile.TemporaryDirectory() as temporary:
            file = Path(temporary) / "arm.elf"
            file.write_bytes(self.arm_elf())
            image, base, entry = build.kernel_image(file, "aarch64")
            self.assertEqual((base, entry), (0x80200000, 0x80200000))
            self.assertEqual(image[:4], b"\x10\0\0\x94")
            self.assertEqual(image[56:60], b"ARM\x64")
            self.assertEqual(len(image), 8192)
            self.assertEqual(image[64:4096], bytes(4096 - 64))
            self.assertEqual(image[5120:], bytes(8192 - 5120))

    def test_arm_rejects_wrong_machine_header_flags_offset_and_entry(self):
        for name, offset, fmt, value in (
            ("x86 machine", 18, "<H", 62),
            ("wrong ELF entry", 24, "<Q", 0x80200000),
            ("wrong branch", 0x1000, "<I", 0),
            ("wrong text offset", 0x1008, "<Q", 0x400000),
            ("oversized declared image", 0x1010, "<Q", 8193),
            ("wrong page size", 0x1018, "<Q", 4),
            ("wrong magic", 0x1038, "<I", 0),
            ("not executable", 64 + 4, "<I", 4),
        ):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                data = bytearray(self.arm_elf())
                struct.pack_into(fmt, data, offset, value)
                file = Path(temporary) / "arm.elf"
                file.write_bytes(data)
                with self.assertRaises(RuntimeError):
                    build.kernel_image(file, "aarch64")

    def test_flatten_preserves_physical_layout_and_zero_fill(self):
        with tempfile.TemporaryDirectory() as temporary:
            file = Path(temporary) / "kernel.elf"
            file.write_bytes(self.elf())
            image, base, entry = build.kernel_image(file)
            self.assertEqual((base, entry), (0x08000000, 0x08001200))
            self.assertEqual(image[:16], bytes(range(16)))
            self.assertEqual(image[16:4096], bytes(4096 - 16))
            self.assertEqual(image[4096:5120], bytes([0xAB]) * 1024)
            self.assertEqual(image[5120:], bytes(8192 - 5120))

    def test_rejects_wrong_architecture_overlaps_and_non_executable_entry(self):
        for name, offset, fmt, value in (
            ("architecture", 18, "<H", 183),
            ("overlap", 64 + 56 + 24, "<Q", 0x08000000),
            ("entry flags", 64 + 56 + 4, "<I", 4),
            ("file larger than memory", 64 + 40, "<Q", 1),
            ("oversized image", 64 + 56 + 40, "<Q", build.MAX_IMAGE_SIZE + 1),
        ):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                data = bytearray(self.elf())
                struct.pack_into(fmt, data, offset, value)
                file = Path(temporary) / "kernel.elf"
                file.write_bytes(data)
                with self.assertRaises(RuntimeError):
                    build.kernel_image(file)
