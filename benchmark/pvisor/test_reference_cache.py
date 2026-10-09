"""A common missing prefix prevents unequal reads of existing Python bytecode."""

import copy
import os
import py_compile
import subprocess
import sys
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path
from unittest import mock

import reference_workload
from reference_baselines import validate_python_cache, validate_tool_cache

POLICY = dict(dont_write_bytecode=True, prefix="/__pvisor_reference_no_pyc__", prefix_exists=False)


def tool_result(mode="filesystem"):
    cache = dict(
        TMPDIR="/work/_reference_tmp",
        HOME="/work/_reference_tmp/reference-home",
        CARGO_HOME="/work/_reference_tmp/reference-cargo",
        NODE_COMPILE_CACHE="/work/_reference_tmp/node-compile-cache",
        NODE_DISABLE_COMPILE_CACHE=None,
        NODE_OPTIONS=None,
    )
    value = dict(mode=mode, workspace="/work", tool_cache=cache)
    if mode == "filesystem":
        value["filesystem"] = {
            name: {"tool_cache": dict(cache), "tool_scratch": "workspace"}
            for name in ("metadata", "read", "write", "git", "rg", "cargo", "npm")
        }
    if mode == "env":
        value["versions"] = {
            "node_compile_cache": dict(
                status="ALREADY_ENABLED",
                directory=cache["NODE_COMPILE_CACHE"] + "/v24.18.0-x64-cf738c9d-1000",
            )
        }
    return value


class ReferenceCacheTests(unittest.TestCase):
    def test_no_write_alone_can_read_old_cache_but_empty_prefix_cannot(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            source = tmp_path / "cached_fixture.py"
            source.write_text("value='cached'\n")
            before = source.stat()
            cached = py_compile.compile(str(source), doraise=True)
            source.write_text("value='source'\n")
            assert source.stat().st_size == before.st_size
            os.utime(source, ns=(before.st_atime_ns, before.st_mtime_ns))
            env = os.environ | {"PYTHONDONTWRITEBYTECODE": "1"}
            env.pop("PYTHONPYCACHEPREFIX", None)
            argv = [sys.executable, "-c", "import cached_fixture; print(cached_fixture.value)"]
            first = subprocess.run(
                argv, cwd=tmp_path, env=env, check=True, capture_output=True, text=True
            )
            assert first.stdout.strip() == "cached"
            prefix = tmp_path / "absent-cache"
            second = subprocess.run(
                argv,
                cwd=tmp_path,
                env=env | {"PYTHONPYCACHEPREFIX": str(prefix)},
                check=True,
                capture_output=True,
                text=True,
            )
            assert second.stdout.strip() == "source"
            assert not prefix.exists()
            assert source.read_text() == "value='source'\n"
            assert os.path.isfile(cached)

    def test_parent_and_all_filesystem_children_must_confirm_policy(self):
        value = {
            "python_cache": POLICY,
            "filesystem": {
                name: {"python_cache": POLICY}
                for name in ("metadata", "read", "write", "git", "rg", "cargo", "npm")
            },
        }
        validate_python_cache(value)
        missing = copy.deepcopy(value)
        del missing["filesystem"]["git"]["python_cache"]
        with self.assertRaisesRegex(ValueError, "filesystem child"):
            validate_python_cache(missing)

    def test_unverified_or_unequal_cache_policy_is_rejected(self):
        for fault in [
            "missing",
            "write-enabled",
            "wrong-prefix",
            "prefix-present",
            "coerced-boolean",
        ]:
            with self.subTest(fault=fault):
                value = {"python_cache": dict(POLICY)}
                if fault == "missing":
                    del value["python_cache"]
                elif fault == "write-enabled":
                    value["python_cache"]["dont_write_bytecode"] = False
                elif fault == "wrong-prefix":
                    value["python_cache"]["prefix"] = None
                elif fault == "prefix-present":
                    value["python_cache"]["prefix_exists"] = True
                else:
                    value["python_cache"]["dont_write_bytecode"] = 1
                with self.assertRaisesRegex(ValueError, "Python payload"):
                    validate_python_cache(value)

    def test_task_local_cache_is_shared_only_within_one_task(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            resources.callback(os.chdir, os.getcwd())
            os.chdir(tmp_path)
            resources.enter_context(
                mock.patch.dict(os.environ, {"PVISOR_REFERENCE_TOOL_SCRATCH": str("workspace")})
            )
            for name, value in dict(
                NODE_DISABLE_COMPILE_CACHE="1",
                NODE_COMPILE_CACHE="/foreign",
                NODE_COMPILE_CACHE_PORTABLE="1",
                NODE_OPTIONS="--jitless",
                PVISOR_REFERENCE_TMPDIR="/dev/shm/foreign",
                TMPDIR="/tmp",
                HOME="/foreign-home",
                CARGO_HOME="/foreign-cargo",
            ).items():
                resources.enter_context(mock.patch.dict(os.environ, {name: str(value)}))
            first = reference_workload.tools_env()
            cache = tmp_path / "_reference_tmp"
            assert first["TMPDIR"] == str(cache)
            assert first["HOME"] == str(cache / "reference-home")
            assert first["CARGO_HOME"] == str(cache / "reference-cargo")
            assert first["NODE_COMPILE_CACHE"] == str(cache / "node-compile-cache")
            assert (
                not set(
                    ("NODE_DISABLE_COMPILE_CACHE", "NODE_OPTIONS", "NODE_COMPILE_CACHE_PORTABLE")
                )
                & first.keys()
            )
            (cache / "private-state").write_text("same task")
            assert reference_workload.tools_env() == first
            second_task = tmp_path / "next-task"
            second_task.mkdir()
            resources.callback(os.chdir, os.getcwd())
            os.chdir(second_task)
            second = reference_workload.tools_env()
            assert second["TMPDIR"] != first["TMPDIR"]
            assert list((second_task / "_reference_tmp").iterdir()) == []

    def test_preexisting_task_cache_is_rejected_before_workload(self):
        for kind in ["directory", "file", "symlink", "dangling-symlink"]:
            with self.subTest(kind=kind):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    resources.callback(os.chdir, os.getcwd())
                    os.chdir(tmp_path)
                    cache = tmp_path / "_reference_tmp"
                    if kind == "directory":
                        cache.mkdir()
                    elif kind == "file":
                        cache.write_text("contamination")
                    else:
                        cache.symlink_to(tmp_path if kind == "symlink" else tmp_path / "absent")
                    resources.enter_context(
                        mock.patch.object(sys, "argv", ["workload", "--mode", "filesystem"])
                    )
                    with self.assertRaisesRegex(ValueError, "absent before"):
                        reference_workload.main()

    def test_cache_cannot_follow_a_foreign_symlink(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            resources.callback(os.chdir, os.getcwd())
            os.chdir(tmp_path)
            resources.enter_context(
                mock.patch.dict(os.environ, {"PVISOR_REFERENCE_TOOL_SCRATCH": str("workspace")})
            )
            (tmp_path / "_reference_tmp").symlink_to(tmp_path / "outside")
            with self.assertRaisesRegex(ValueError, "symlink"):
                reference_workload.tools_env()

    def test_correct_task_cache_policy_is_accepted(self):
        for mode in ["filesystem", "tools", "env"]:
            with self.subTest(mode=mode):
                validate_tool_cache(tool_result(mode))

    def test_unequal_or_unverified_task_cache_is_rejected(self):
        for fault in [
            "no-workspace",
            "relative-workspace",
            "no-cache",
            "global-tmp",
            "global-home",
            "global-cargo",
            "node-disabled",
            "global-node",
            "node-options",
            "missing-child",
            "unequal-child",
        ]:
            with self.subTest(fault=fault):
                value = tool_result()
                if fault == "no-workspace":
                    del value["workspace"]
                elif fault == "relative-workspace":
                    value["workspace"] = "work"
                elif fault == "no-cache":
                    del value["tool_cache"]
                elif fault == "missing-child":
                    del value["filesystem"]["npm"]["tool_cache"]
                elif fault == "unequal-child":
                    value["filesystem"]["npm"]["tool_cache"]["NODE_COMPILE_CACHE"] = "/tmp/cache"
                else:
                    field, replacement = {
                        "global-tmp": ("TMPDIR", "/tmp"),
                        "global-home": ("HOME", "/root"),
                        "global-cargo": ("CARGO_HOME", "/tmp/cargo"),
                        "node-disabled": ("NODE_DISABLE_COMPILE_CACHE", "1"),
                        "global-node": ("NODE_COMPILE_CACHE", "/tmp/cache"),
                        "node-options": ("NODE_OPTIONS", "--jitless"),
                    }[fault]
                    value["tool_cache"][field] = replacement
                with self.assertRaises(ValueError):
                    validate_tool_cache(value)

    def test_actual_node_capability_is_required(self):
        for fault in [
            "missing",
            "disabled",
            "different-directory",
            "parent-escape",
            "arbitrary-child",
            "nested-child",
        ]:
            with self.subTest(fault=fault):
                value = tool_result("env")
                if fault == "missing":
                    del value["versions"]["node_compile_cache"]
                elif fault == "disabled":
                    value["versions"]["node_compile_cache"]["status"] = "DISABLED"
                elif fault == "different-directory":
                    value["versions"]["node_compile_cache"]["directory"] = "/tmp/cache"
                else:
                    suffix = {
                        "parent-escape": "/../v24.18.0-x64-hash-1000",
                        "arbitrary-child": "/foreign",
                        "nested-child": "/nested/v24.18.0-x64-hash-1000",
                    }[fault]
                    value["versions"]["node_compile_cache"]["directory"] = (
                        value["tool_cache"]["NODE_COMPILE_CACHE"] + suffix
                    )
                with self.assertRaisesRegex(ValueError, "actual Node"):
                    validate_tool_cache(value)

    def test_executor_cache_preserves_tmpdir_and_starts_fresh_per_task(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            parent = tmp_path / "executor-tmp"
            parent.mkdir()
            work = tmp_path / "work"
            work.mkdir()
            resources.callback(os.chdir, os.getcwd())
            os.chdir(work)
            resources.enter_context(mock.patch.dict(os.environ, {"TMPDIR": str(str(parent))}))
            resources.enter_context(
                mock.patch.dict(os.environ, {"PVISOR_REFERENCE_TOOL_SCRATCH": str("executor")})
            )
            first = reference_workload.tools_env()
            private = Path(first["TMPDIR"])
            assert private.parent == parent / ".data"
            assert private.name.startswith("pvisor-reference-")
            assert list(private.iterdir()) == []
            assert not (work / "_reference_tmp").exists()
            (private / "retained-state").write_text("only this task")
            assert reference_workload.tools_env()["TMPDIR"] == str(private)
            next_work = tmp_path / "next"
            next_work.mkdir()
            resources.callback(os.chdir, os.getcwd())
            os.chdir(next_work)
            second = Path(reference_workload.tools_env()["TMPDIR"])
            assert second != private and second.parent == private.parent
            assert list(second.iterdir()) == []

    def test_nested_tool_action_reuses_only_explicit_private_cache(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            private = tmp_path / ".data/pvisor-reference-abcdefgh"
            private.mkdir(parents=True)
            (private / "same-task").write_text("private")
            resources.callback(os.chdir, os.getcwd())
            os.chdir(tmp_path)
            resources.enter_context(
                mock.patch.dict(os.environ, {"PVISOR_REFERENCE_TOOL_SCRATCH": str("executor")})
            )
            resources.enter_context(
                mock.patch.dict(os.environ, {"PVISOR_REFERENCE_CACHE_DIRECTORY": str(str(private))})
            )
            assert reference_workload.tools_env()["TMPDIR"] == str(private)
            assert (private / "same-task").read_text() == "private"

    def test_invalid_executor_scratch_is_rejected(self):
        for fault in [
            "relative",
            "foreign-name",
            "symlink-parent",
            "unknown-policy",
            "inherited-foreign",
        ]:
            with self.subTest(fault=fault):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    resources.callback(os.chdir, os.getcwd())
                    os.chdir(tmp_path)
                    resources.enter_context(
                        mock.patch.dict(
                            os.environ, {"PVISOR_REFERENCE_TOOL_SCRATCH": str("executor")}
                        )
                    )
                    resources.enter_context(
                        mock.patch.dict(os.environ, {"TMPDIR": str(str(tmp_path))})
                    )
                    if fault == "relative":
                        resources.enter_context(
                            mock.patch.dict(os.environ, {"TMPDIR": str("relative")})
                        )
                    elif fault == "foreign-name":
                        resources.enter_context(
                            mock.patch.dict(
                                os.environ, {"PVISOR_REFERENCE_CACHE_DIRECTORY": str(str(tmp_path))}
                            )
                        )
                    elif fault == "symlink-parent":
                        (tmp_path / ".data").symlink_to(tmp_path / "foreign")
                    elif fault == "unknown-policy":
                        resources.enter_context(
                            mock.patch.dict(
                                os.environ, {"PVISOR_REFERENCE_TOOL_SCRATCH": str("unknown")}
                            )
                        )
                    else:
                        resources.enter_context(
                            mock.patch.dict(
                                os.environ,
                                {"PVISOR_REFERENCE_CACHE_DIRECTORY": str("/tmp/foreign-cache")},
                            )
                        )
                    with self.assertRaises(ValueError):
                        reference_workload.tools_env()

    def test_new_jobs_cannot_accept_missing_or_wrong_scratch_policy(self):
        value = tool_result()
        with self.assertRaisesRegex(ValueError, "declared experiment"):
            validate_tool_cache(value, "executor")
        value["tool_scratch"] = "workspace"
        validate_tool_cache(value, "workspace")
        del value["filesystem"]["npm"]["tool_scratch"]
        with self.assertRaisesRegex(ValueError, "child scratch policy"):
            validate_tool_cache(value, "workspace")

    def test_executor_policy_requires_private_directory_and_actual_storage_type(self):
        value = tool_result("env")
        value["tool_scratch"] = "executor"
        old = value["tool_cache"]["TMPDIR"]
        new = "/.pvisor-tmp-run-test/.data/pvisor-reference-abcdefgh"
        value["tool_cache"] = {
            k: v.replace(old, new) if isinstance(v, str) else v
            for k, v in value["tool_cache"].items()
        }
        probe = value["versions"]["node_compile_cache"]
        probe["directory"] = probe["directory"].replace(old, new)
        probe["filesystem_type"] = 0x01021994
        validate_tool_cache(value, "executor")
        probe["filesystem_type"] = None
        with self.assertRaisesRegex(ValueError, "storage type"):
            validate_tool_cache(value, "executor")
        probe["filesystem_type"] = 0x01021994
        value["tool_cache"]["TMPDIR"] = "/tmp"
        with self.assertRaisesRegex(ValueError, "fresh private"):
            validate_tool_cache(value, "executor")
