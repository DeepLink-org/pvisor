"""Adapter feedback/error contracts, without starting a VM."""

import json
import os
import subprocess
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path
from unittest.mock import patch

from executor import EXAMPLE, ExecutionError, execute


class ExecutorTests(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.stack.enter_context(patch.dict(os.environ, {"WORK_ROOT": self.temp.name}))
        self.stack.enter_context(patch("executor.shutil.which", return_value="/bin/pvisor"))
        self.run = self.stack.enter_context(patch("executor.subprocess.run"))
        self.run.return_value.returncode = 0
        self.task = json.loads((EXAMPLE / "task.json").read_text())
        self.task["task_id"] = "framework-assigned-task"
        self.steps = [
            {**step, "exit_code": 0, "stdout": "ok", "stderr": "", "timed_out": False}
            for step in self.task["steps"]
        ]
        self.bundle = {
            "run": {"state": "completed", "exit_code": 0, "run_id": "job-1", "attempt_id": "a-1"}
        }
        self.calls = []
        self.cli = self.stack.enter_context(patch("executor.subprocess.check_output", self.output))

    def output(self, args, **kwargs):
        self.calls.append(args[1])
        if args[1] == "status":
            return json.dumps(self.bundle)
        if args[1] == "inspect":
            return json.dumps({"steps": self.steps})
        return ""

    def execute(self):
        return execute(self.task, EXAMPLE / "candidate.patch")

    def test_caller_input_and_feedback_survive_roundtrip(self):
        feedback = self.execute()
        root = Path(feedback["records_dir"])
        self.assertEqual(json.loads((root / "workspace/task.json").read_text()), self.task)
        self.assertEqual(feedback["task_id"], self.task["task_id"])
        self.assertEqual(feedback["steps"], self.steps)
        self.assertEqual(feedback["job_id"], "job-1")
        self.assertEqual(json.loads((root / "sample.json").read_text()), feedback)
        self.assertIn("drop", self.calls)

    def test_tool_failure_and_timeout_return_observations_without_drop(self):
        for exit_code, timed_out in ((2, False), (None, True)):
            with self.subTest(exit_code=exit_code):
                self.calls.clear()
                self.steps[0].update(exit_code=exit_code, timed_out=timed_out, stderr="feedback")
                feedback = self.execute()
                self.assertEqual(feedback["steps"], self.steps)
                self.assertNotIn("drop", self.calls)

    def test_guest_preparation_failure_is_execution_error(self):
        self.bundle["run"]["exit_code"] = 1
        with self.assertRaises(ExecutionError):
            self.execute()
        self.assertNotIn("inspect", self.calls)
        self.assertNotIn("drop", self.calls)
        self.assertEqual(len(list(Path(self.temp.name).glob("*/bundle.json"))), 1)

    def test_missing_feedback_is_execution_error(self):
        self.steps = []
        with self.assertRaises(ExecutionError):
            self.execute()
        self.assertNotIn("drop", self.calls)

    def test_startup_failure_without_bundle_is_execution_error(self):
        self.run.return_value.returncode = 1
        with patch(
            "executor.subprocess.check_output", side_effect=subprocess.CalledProcessError(1, [])
        ):
            with self.assertRaises(ExecutionError):
                self.execute()
        self.assertEqual(len(list(Path(self.temp.name).glob("*/workspace/task.json"))), 1)


if __name__ == "__main__":
    unittest.main()
