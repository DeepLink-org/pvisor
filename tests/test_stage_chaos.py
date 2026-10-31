"""Safety/oracle contracts for the stage driver; no VM or firmware required."""

import json
import os
import stat
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

from tests import stage_chaos as chaos
from tests.fixtures import workspace


class SafetyTests(unittest.TestCase):
    def test_owner_mode_and_symlink_rejected(self):
        with workspace() as (root, _):
            file = root / "binary"
            file.write_text("frozen")
            file.chmod(0o700)
            real_lstat = Path.lstat

            # /tmp is intentionally rejected; mock only ancestor metadata so the
            # test workspace can exercise the leaf's real permissions/links.
            def metadata(path):
                if path == file or path == root / "link":
                    return real_lstat(path)
                return os.stat_result((stat.S_IFDIR | 0o755, 1, 1, 1, os.getuid(), 0, 0, 0, 0, 0))

            with patch.object(Path, "lstat", metadata):
                self.assertEqual(chaos.safe_ancestors(file), file)
                file.chmod(0o720)
                with self.assertRaises(ValueError):
                    chaos.safe_ancestors(file)
                file.chmod(0o700)
                (root / "link").symlink_to(file)
                with self.assertRaises(ValueError):
                    chaos.safe_ancestors(root / "link")
            with patch.object(
                Path,
                "lstat",
                return_value=os.stat_result(
                    (stat.S_IFDIR | 0o755, 1, 1, 1, os.getuid() + 10001, 0, 0, 0, 0, 0)
                ),
            ):
                with self.assertRaises(ValueError):
                    chaos.safe_ancestors(file)

    def test_world_writable_sticky_ancestor_rejected(self):
        with workspace() as (root, _):
            with self.assertRaises(ValueError):
                chaos.safe_ancestors(root)

    def test_fresh_directory_and_delete_guards(self):
        with workspace() as (root, _):
            # Ownership policy is independently tested above. Only bypass /tmp.
            with patch.object(chaos, "safe_ancestors", side_effect=lambda path: Path(path)):
                output = chaos.fresh_output(root / "evidence")
                with self.assertRaises(FileExistsError):
                    chaos.fresh_output(output)
                case = output / "case-0000001"
                case.mkdir(mode=0o700)
                external = root / "external"
                external.mkdir()
                (external / "sentinel").write_text("untouched")
                (case / "link").symlink_to(external, target_is_directory=True)
                for unsafe in [root, external, output, output / "other"]:
                    with self.assertRaises((AssertionError, FileNotFoundError)):
                        chaos.delete_case(output, unsafe)
                chaos.delete_case(output, case)
                self.assertEqual((external / "sentinel").read_text(), "untouched")
                case.symlink_to(external, target_is_directory=True)
                with self.assertRaises(AssertionError):
                    chaos.delete_case(output, case)
                case.unlink()
                case.mkdir(mode=0o755)
                with self.assertRaises(AssertionError):
                    chaos.delete_case(output, case)
                (output / chaos.MARKER).unlink()
                with self.assertRaises(AssertionError):
                    chaos.delete_case(output, case)

    def test_snapshot_no_follow_content_mode_and_identity(self):
        with workspace() as (root, _):
            tree = root / "tree"
            tree.mkdir()
            file = tree / "file"
            file.write_text("one")
            (tree / "link").symlink_to(root / "missing")
            before = chaos.snapshot(tree)
            self.assertEqual(before, chaos.snapshot(tree))
            self.assertIn("inode", before["file"])
            self.assertIn("ctime_ns", before["file"])
            self.assertNotIn("atime_ns", before["file"])
            self.assertEqual(before["link"]["link"], str(root / "missing"))
            file.chmod(0o600)
            self.assertNotEqual(before, chaos.snapshot(tree))
            file.write_text("two")
            self.assertNotEqual(before["file"]["sha256"], chaos.snapshot(tree)["file"]["sha256"])

    def test_deadline_and_timeout_caps(self):
        self.assertEqual(chaos.remaining_timeout(100, 80, now=0), 30)
        self.assertEqual(chaos.remaining_timeout(100, 80, now=79), 1)
        with self.assertRaises(chaos.BudgetExpired):
            chaos.remaining_timeout(100, 80, now=80)
        for seconds in ["nan", "inf", "0", "-1", "86401"]:
            with self.assertRaises(SystemExit):
                chaos.main(
                    [
                        "--binary",
                        "unused",
                        "--firmware",
                        "unused",
                        "--output",
                        "unused",
                        "--seed",
                        "1",
                        "--seconds",
                        seconds,
                    ]
                )

    def test_summary_counts_and_aborted_case_without_vm(self):
        with workspace() as (root, _):

            def driver(name):
                options = chaos.parser().parse_args(
                    [
                        "--binary",
                        str(root / "unused"),
                        "--firmware",
                        str(root / "unused"),
                        "--output",
                        str(root / name),
                        "--seed",
                        "17",
                        "--seconds",
                        "10",
                        "--rounds",
                        "2",
                    ]
                )
                return chaos.Driver(options)

            with patch.object(chaos, "safe_ancestors", side_effect=lambda path: Path(path)):
                normal = driver("normal")
                with (
                    patch.object(normal, "setup"),
                    patch.object(normal, "run_case", return_value="pass"),
                    patch("builtins.print"),
                ):
                    self.assertEqual(normal.run(), 0)
                summary = json.loads((normal.root / "summary.json").read_text())
                self.assertEqual(summary["completed_rounds"], 2)
                self.assertEqual(summary["round_counts"]["chain"], 1)
                self.assertEqual(summary["round_counts"]["conflict"], 1)
                self.assertFalse(summary["aborted"])
                self.assertLessEqual(summary["active_elapsed"], summary["wall_elapsed"])
                aborted = driver("aborted")
                with (
                    patch.object(aborted, "setup"),
                    patch.object(
                        aborted, "run_case", side_effect=chaos.BudgetExpired("case deadline")
                    ),
                    patch("builtins.print"),
                ):
                    self.assertEqual(aborted.run(), 1)
                summary = json.loads((aborted.root / "summary.json").read_text())
                self.assertTrue(summary["aborted"])
                self.assertEqual(summary["completed_rounds"], 0)
                self.assertEqual(summary["attempted_rounds"], 1)
                self.assertEqual(summary["fault_injection"], "not_injected")
                incomplete = driver("incomplete")
                with (
                    patch.object(incomplete, "setup"),
                    patch.object(incomplete, "run_case", side_effect=["miss", "unknown_verified"]),
                    patch("builtins.print"),
                ):
                    self.assertEqual(incomplete.run(), 2)
                summary = json.loads((incomplete.root / "summary.json").read_text())
                self.assertEqual(summary["completed_rounds"], 0)
                self.assertEqual(summary["finished_rounds"], 2)
                self.assertEqual(summary["outcomes"]["miss"], 1)
                capped = driver("capped")
                capped.last_progress -= 61
                with patch("builtins.print") as progress:
                    capped.progress()
                    self.assertTrue(progress.call_args.kwargs["flush"])
                capped.options.max_evidence_mib = 0
                with self.assertRaises(chaos.BudgetExpired):
                    capped.emit("oracle", actual="payload")
                capped.events.close()

    def test_time_budget_finishes_between_cases_without_fabricating_a_pass(self):
        with workspace() as (root, _):
            options = chaos.parser().parse_args(
                [
                    "--binary",
                    "unused",
                    "--firmware",
                    "unused",
                    "--output",
                    str(root / "budget"),
                    "--seed",
                    "1",
                    "--seconds",
                    "10",
                ]
            )
            with patch.object(chaos, "safe_ancestors", side_effect=lambda path: Path(path)):
                driver = chaos.Driver(options)
                with (
                    patch.object(driver, "setup"),
                    patch.object(driver, "run_case", return_value="pass") as run_case,
                    patch("builtins.print"),
                ):
                    self.assertEqual(driver.run(), 0)
                summary = json.loads((driver.root / "summary.json").read_text())
                self.assertEqual(run_case.call_count, 1)
                self.assertEqual(summary["completed_rounds"], 1)
                self.assertFalse(summary["aborted"])
                self.assertIn(
                    '"event": "budget_complete"', (driver.root / "evidence.jsonl").read_text()
                )

    @unittest.skipUnless(sys.platform == "linux", "Linux subreaper/pidfd contract")
    def test_subreaper_cleans_detached_adopted_child_without_group_kill(self):
        with workspace() as (root, _):
            child = (
                "import os,time; os.setsid(); print('DETACHED',os.getpid(),flush=True); "
                "os.close(1); os.close(2); time.sleep(60)"
            )
            parent = (
                "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c',"
                + repr(child)
                + "]); time.sleep(0.15)"
            )
            result = chaos.run_bounded(
                [sys.executable, "-c", parent], root, {"PATH": "/usr/bin:/bin"}, 3
            )
            self.assertFalse(result["timed_out"])
            self.assertTrue(result["cleanup"]["complete"])
            pid = int(result["stdout"].split("DETACHED ")[1].split()[0])
            self.assertIsNone(chaos.proc_identity(pid))
            sent = [item for item in result["cleanup"]["signals"] if item["pid"] == pid]
            self.assertTrue(sent)
            self.assertTrue(all(item["starttime"] > 0 for item in sent))

    @unittest.skipUnless(sys.platform == "linux", "Linux subreaper/pidfd contract")
    def test_pid_starttime_guard_and_bounded_logfiles(self):
        with workspace() as (root, _):
            scope = chaos.Descendants()
            command = None
            try:
                command = chaos.WatchedCommand(
                    [
                        sys.executable,
                        "-c",
                        "import time; print('ready',flush=True); time.sleep(60)",
                    ],
                    root,
                    {"PATH": "/usr/bin:/bin"},
                    scope,
                    root / "child",
                )
                stale = dict(command.identity, starttime=command.identity["starttime"] + 1)
                self.assertFalse(scope.send(stale, chaos.signal.SIGKILL))
                self.assertIsNone(command.process.poll())
                own = chaos.proc_identity(os.getpid())
                with self.assertRaises(AssertionError):
                    scope.send(own, chaos.signal.SIGTERM)
                self.assertTrue(scope.cleanup(timeout=3)["complete"])
                command.finish(chaos.time.monotonic() + 1)
                self.assertLessEqual((root / "child.stdout").stat().st_size, chaos.LIMIT)
            finally:
                scope.cleanup(timeout=3)
                if command:
                    command.close()
                scope.close()

    def test_completion_unknown_is_not_cancelled(self):
        run = dict(state="cancelled", run_id="r", attempt_id="a", finished_at_unix_ms=1)
        completion = dict(result=dict(run), bundle_blake3="a" * 64)
        self.assertTrue(chaos.committed_completion(run, completion))
        self.assertFalse(chaos.committed_completion(run, None))
        completion["result"]["attempt_id"] = "another"
        self.assertFalse(chaos.committed_completion(run, completion))
        completion["result"]["attempt_id"] = "a"
        completion["result"]["failure"] = {"kind": "unknown"}
        self.assertFalse(chaos.committed_completion(run, completion))

    @unittest.skipUnless(sys.platform == "linux", "Linux subreaper/pidfd contract")
    def test_watched_files_actually_cap_large_output(self):
        with workspace() as (root, _):
            scope = chaos.Descendants()
            command = None
            try:
                command = chaos.WatchedCommand(
                    [
                        sys.executable,
                        "-c",
                        "import os; os.write(1,b'x'*100000); os.write(2,b'y'*100000)",
                    ],
                    root,
                    {"PATH": "/usr/bin:/bin"},
                    scope,
                    root / "log",
                )
                result = command.finish(chaos.time.monotonic() + 3)
                self.assertFalse(result["timed_out"])
                for name in ("stdout", "stderr"):
                    self.assertEqual(result["bytes"][name], 100000)
                    self.assertEqual((root / ("log." + name)).stat().st_size, chaos.LIMIT)
            finally:
                self.assertTrue(scope.cleanup(timeout=3)["complete"])
                if command:
                    command.close()
                scope.close()

    @unittest.skipUnless(sys.platform == "linux", "Linux subreaper/pidfd contract")
    def test_bounded_capture_json_and_timeout(self):
        with workspace() as (root, _):

            def run(script, timeout=3):
                return chaos.run_bounded(
                    [sys.executable, "-c", script], root, {"PATH": "/usr/bin:/bin"}, timeout
                )

            obj = {"schema_version": 1, "selected": [], "status": "ready"}
            result = run("print(" + repr(json.dumps(obj)) + ")")
            self.assertEqual(chaos.apply_object(result, 0, "ready"), obj)
            with self.assertRaises(AssertionError):
                chaos.apply_object(result, 3)
            result = run("import os; os.write(1, b'x'*100000); os.write(2, b'y'*100000)")
            self.assertEqual(result["bytes"], {"stdout": 100000, "stderr": 100000})
            self.assertEqual(len(result["stdout"]), chaos.LIMIT)
            self.assertTrue(result["truncated"]["stderr"])
            self.assertIsNone(result["json"])
            self.assertIsNone(run("print('not json')")["json"])
            result = run("import time; time.sleep(10)", timeout=0.1)
            self.assertTrue(result["timed_out"])
            self.assertLess(result["elapsed"], 3)


if __name__ == "__main__":
    unittest.main()
