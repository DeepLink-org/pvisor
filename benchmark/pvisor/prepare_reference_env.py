#!/usr/bin/env python3
"""Build one identical, offline Agent environment for pVisor, Docker and reference VMs.

The tools rootfs comes from product_v1's environment preparation. The local
Claude/Codex installations and Rust sysroot are copied, never authenticated.
"""

import argparse
import hashlib
import json
import shutil
import subprocess
import time
from pathlib import Path
from types import SimpleNamespace

from run_all import ldd_paths, prepare_rootfs
from v1.filesystem import fixture
from reference_inputs import create_reference_manifest


def prepare_local_tools(root, binary, toolchain):
    """Build the Fedora 44 tool base directly from installed, offline host tools."""
    root.mkdir()
    prepare_rootfs(root, binary)
    sources = set()
    for name in ("python3", "git", "rg", "node-24", "cc", "as", "ld"):
        path = Path("/usr/bin") / name
        if not path.is_file():
            raise ValueError(
                f"missing local Fedora tool: {path}; supply --tools-rootfs for another layout"
            )
        sources.add(path)
        sources.update(ldd_paths(path))
    for name in ("rustc", "cargo"):
        sources.update(ldd_paths(toolchain / "bin" / name))
    for pattern in ("*crt*.o", "libc*.so", "libc_nonshared.a"):
        sources.update(Path("/usr/lib64").glob(pattern))
    for name in (
        "libutil.a",
        "librt.a",
        "libpthread.a",
        "libdl.a",
        "libgcc_s.so.1",
        "libutil.so.1",
        "librt.so.1",
        "libpthread.so.0",
        "libm.so",
        "libm.so.6",
        "libmvec.so.1",
    ):
        sources.add(Path("/usr/lib64") / name)
    for path in sources:
        target = root / path.relative_to("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path.resolve(), target)
        if path.parent == Path("/usr/lib64"):
            alias = root / "lib64" / path.name
            alias.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path.resolve(), alias)
    for directory in (
        Path("/usr/lib64/python3.14"),
        Path("/usr/lib/node_modules_24"),
        Path("/usr/lib/gcc"),
        Path("/usr/libexec/gcc"),
    ):
        shutil.copytree(
            directory,
            root / directory.relative_to("/"),
            symlinks=True,
            ignore=shutil.ignore_patterns("site-packages", "__pycache__", "32"),
        )
    (root / "usr/bin/node").symlink_to("node-24")
    (root / "usr/bin/npm").symlink_to("../lib/node_modules_24/npm/bin/npm-cli.js")
    (root / "tmp").chmod(0o1777)
    (root / "root").mkdir()


def tool_identities(root):
    paths = {
        name: path
        for name, path in [
            ("python", "usr/bin/python3"),
            ("node", "usr/bin/node"),
            ("git", "usr/bin/git"),
            ("rg", "usr/bin/rg"),
            ("rustc", "opt/toolchain/bin/rustc"),
            ("cargo", "opt/toolchain/bin/cargo"),
            ("claude", "usr/local/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe"),
            ("codex_launcher", "usr/local/lib/node_modules/@openai/codex/bin/codex.js"),
        ]
    }
    for index, path in enumerate(
        sorted((root / "usr/local/lib/node_modules/@openai/codex").rglob("bin/codex"))
    ):
        paths[f"codex_native_{index}"] = str(path.relative_to(root))
    result = {}
    for name, relative in paths.items():
        digest = hashlib.sha256()
        with (root / relative).open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        result[name] = {"path": relative, "sha256": digest.hexdigest()}
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--tools-rootfs", type=Path)
    p.add_argument("--binary", type=Path, default=Path("target/release/pvisor"))
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--kernel-elf", type=Path, required=True)
    p.add_argument("--kernel-bzimage", type=Path, required=True)
    p.add_argument("--kernel-config", type=Path, required=True)
    p.add_argument("--docker-host", required=True)
    args = p.parse_args()
    R = Path(__file__).resolve().parents[2]
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    root = out / "rootfs"
    start = time.perf_counter_ns()
    toolchain = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    if args.tools_rootfs:
        subprocess.run(
            ["cp", "--reflink=auto", "-a", str(args.tools_rootfs.resolve()), str(root)], check=True
        )
    else:
        prepare_local_tools(root, args.binary.resolve(), toolchain)
    for path in (
        Path("/usr/local/lib/node_modules/@anthropic-ai/claude-code"),
        Path("/usr/local/lib/node_modules/@openai/codex"),
    ):
        target = root / path.relative_to("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["cp", "--reflink=auto", "-a", str(path), str(target)], check=True)
    for name in ("claude", "codex"):
        p = root / "usr/local/bin" / name
        p.parent.mkdir(parents=True, exist_ok=True)
        p.symlink_to(
            "../lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe"
            if name == "claude"
            else "../lib/node_modules/@openai/codex/bin/codex.js"
        )
    (root / "usr/local/bin/node").symlink_to("/usr/bin/node")
    (root / "opt/toolchain").mkdir(parents=True)
    subprocess.run(
        [
            "cp",
            "--reflink=auto",
            "-a",
            subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip() + "/.",
            str(root / "opt/toolchain"),
        ],
        check=True,
    )
    (root / "bench").mkdir(exist_ok=True)
    shutil.copy2(R / "benchmark/pvisor/reference_workload.py", root / "bench/reference_workload.py")
    shutil.copytree(
        R / "benchmark/pvisor", root / "bench/harness", ignore=shutil.ignore_patterns("__pycache__", ".data")
    )
    # Python extensions load libraries on demand; interpreter ldd alone is insufficient.
    extensions = list((root / "usr/lib64").glob("python*/lib-dynload/*.so"))
    libraries = set()
    for extension in extensions:
        result = subprocess.run(["ldd", str(extension)], capture_output=True, text=True, check=True)
        for word in result.stdout.split():
            if word.startswith("/") and Path(word).is_file():
                libraries.add(Path(word))
    for library in libraries:
        target = root / library.relative_to("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(library.resolve(), target)
    for path in root.rglob("*"):
        if not path.is_symlink() and path.is_file() and path.stat().st_mode & 0o6000:
            path.chmod(path.stat().st_mode & ~0o6000)
    shutil.copy2(root / "bin/sh", root / "bin/bash")
    shutil.copy2(root / "bin/sh", root / "usr/bin/bash")
    subprocess.run(
        [
            "rustc",
            "--edition",
            "2021",
            "--target",
            "x86_64-unknown-linux-musl",
            "-C",
            "opt-level=2",
            "-C",
            "panic=abort",
            "-C",
            "strip=symbols",
            str(R / "benchmark/pvisor/reference_guest_init.rs"),
            "-o",
            str(root / "bench/init"),
        ],
        check=True,
    )
    subprocess.run(
        [
            "rustc",
            "--edition",
            "2021",
            "--target",
            "x86_64-unknown-linux-musl",
            "-C",
            "opt-level=2",
            "-C",
            "panic=abort",
            "-C",
            "strip=symbols",
            str(R / "benchmark/pvisor/reference_affinity.rs"),
            "-o",
            str(root / "bench/affinity"),
        ],
        check=True,
    )
    for name, src in [
        ("vmlinux", args.kernel_elf),
        ("bzImage", args.kernel_bzimage),
        ("kernel.config", args.kernel_config),
    ]:
        shutil.copy2(src, out / name)
    work = root / "work"
    work.mkdir()
    (work / "python").mkdir()
    (work / "rust/src").mkdir(parents=True)
    (work / "node/local").mkdir(parents=True)
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
        p = work / f"node/local/p{i}"
        p.mkdir()
        (p / "package.json").write_text(json.dumps({"name": f"p{i}", "version": "1.0.0"}))
        (p / "index.js").write_text(f"module.exports = {i};\n")
    (work / "node/test.js").write_text(
        "const assert = require('assert'); for(let i=0;i<32;i++) assert.equal(require(`p${i}`),i); console.log('NODE_GRADE_PASS');\n"
    )
    for name in ("git", "rg"):
        assert (root / "usr/bin" / name).exists()
    subprocess.run(["git", "init", "-q"], cwd=work, check=True)
    subprocess.run(["git", "add", "."], cwd=work, check=True)
    subprocess.run(
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
        cwd=work,
        check=True,
    )
    (root / "etc/hosts").write_text("127.0.0.1 localhost\n")
    (root / "tmp").chmod(0o1777)
    fs_input = fixture(SimpleNamespace(output=out, toolchain=Path("/opt/toolchain")))
    shutil.move(str(fs_input), root / "work/_fs")
    (out / "tool-identities.json").write_text(json.dumps(tool_identities(root), indent=2) + "\n")
    tar = out / "agent-env.tar"
    subprocess.run(
        ["tar", "-C", str(root), "--owner=0", "--group=0", "-cf", str(tar), "."], check=True
    )
    image = subprocess.check_output(
        ["docker", "--host", args.docker_host, "import", str(tar)], text=True
    ).strip()
    disk = out / "agent-env.ext4"
    subprocess.run(["truncate", "-s", "6G", str(disk)], check=True)
    subprocess.run(["mke2fs", "-q", "-t", "ext4", "-F", "-d", str(root), str(disk)], check=True)
    metadata = {
        "setup_wall_ms": (time.perf_counter_ns() - start) / 1e6,
        "docker_image": image,
        "rootfs": str(root),
        "disk": str(disk),
        "kernel": str(out / "vmlinux"),
        "rootfs_file_bytes": sum(p.stat().st_size for p in root.rglob("*") if p.is_file()),
        "files": sum(1 for p in root.rglob("*") if p.is_file()),
        "workload_sha256": hashlib.sha256(
            (root / "bench/reference_workload.py").read_bytes()
        ).hexdigest(),
        "init_sha256": hashlib.sha256((root / "bench/init").read_bytes()).hexdigest(),
        "fixture_git_commit": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=work, text=True
        ).strip(),
        "kernel_config_sha256": hashlib.sha256((out / "kernel.config").read_bytes()).hexdigest(),
        "preparation_scope": "offline copying installed tools, image import and ext4 creation; excludes package download and kernel compilation",
        "setup_wall_ms_scope": "through image/ext4 preparation; input manifest hashing is recorded separately",
    }
    (out / "assets.json").write_text(json.dumps(metadata, indent=2) + "\n")
    manifest_started = time.perf_counter_ns()
    manifest = create_reference_manifest(out)
    (out / "input-manifest-build.json").write_text(json.dumps({
        "input_manifest": str(manifest),
        "hashing_ms": (time.perf_counter_ns() - manifest_started) / 1e6,
        "scope": "offline input identity generation, excluded from task timers and setup_wall_ms",
    }, indent=2) + "\n")
    print(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    main()
