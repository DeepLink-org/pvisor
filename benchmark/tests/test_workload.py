"""Transport-free contracts for model-free workload replay."""

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "replay"))
# Other benchmark suites import a different module named workload. Load this
# runner by its file path so combined unittest discovery cannot reuse it.
spec = importlib.util.spec_from_file_location("rollout_workload", ROOT / "replay/workload.py")
w = importlib.util.module_from_spec(spec)
spec.loader.exec_module(w)
from workload_backends import build_backend  # noqa: E402


def plan():
    return dict(
        schema="command-replay/v1",
        initial_check="check-environment",
        rounds=[
            dict(
                round=1, wait_s=7, source_outcome="completed", command="first", source_returncode=0
            ),
            dict(round=2, wait_s=3, source_outcome="not_executed"),
            dict(
                round=3,
                wait_s=None,
                source_outcome="timeout",
                command="last",
                source_returncode=124,
            ),
        ],
        official=dict(command="evaluate", read_results_command="parse", required_tests=["a", "b"]),
    )


class Clock:
    now = 0

    def __call__(self):
        return self.now

    def sleep(self, value):
        self.now += value


class Backend:
    def __init__(self, clock, fail=None):
        self.clock = clock
        self.fail = fail
        self.calls = []

    def start(self):
        self.calls.append("create")
        self.clock.sleep(2)
        if self.fail == "create":
            raise RuntimeError("secret must not be recorded")

    def exec(self, command):
        self.calls.append(command)
        self.clock.sleep(1)
        if command == self.fail:
            raise RuntimeError("secret must not be recorded")
        return dict(
            return_code=2 if command == "last" else 0,
            stdout='{"a":"PASSED","b":"FAILED"}' if command == "parse" else "",
            stderr="",
        )

    def stop(self):
        self.calls.append("delete")
        self.clock.sleep(4)
        if self.fail == "delete":
            raise RuntimeError("cleanup failure")
        return dict(api_deleted=True, physical_exit_verified=False)


class WorkloadTests(unittest.TestCase):
    def run_case(self, fail=None, subject=None):
        clock = Clock()
        backend = Backend(clock, fail)
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "new"
            result = w.run(subject or plan(), backend, out, clock=clock, sleep=clock.sleep)
            self.assertEqual(result, json.loads((out / "result.json").read_text()))
        return result, backend

    def test_waits_skip_and_source_timeout_are_preserved(self):
        r, b = self.run_case()
        self.assertEqual(b.calls[-4:], ["last", "evaluate", "parse", "delete"])
        self.assertEqual(r["timings"]["model_wait_s"], 10)
        self.assertEqual(r["timings"]["tool_s"], 2)
        self.assertFalse(r["steps"][1]["executed"])
        self.assertEqual(r["steps"][2]["source_outcome"], "timeout")
        self.assertFalse(r["steps"][2]["exit_matches"])
        self.assertTrue(r["execution_completed"])

    def test_total_includes_creation_initialization_evaluation_and_deletion(self):
        r, _ = self.run_case()
        t = r["timings"]
        self.assertEqual(t["total_s"], 23)
        self.assertEqual(
            t["total_s"],
            sum(t[n] for n in ["create_s", "initialize_s", "replay_s", "official_s", "cleanup_s"]),
        )
        self.assertEqual(t["cleanup_s"], 4)
        self.assertFalse(r["cleanup"]["physical_exit_verified"])

    def test_official_assertion_failure_is_not_infrastructure_failure(self):
        r, _ = self.run_case()
        self.assertEqual(r["official"]["result_class"], "tests_failed")
        self.assertTrue(r["evaluator_healthy"])
        self.assertFalse(r["official"]["passed"])

    def test_creation_failure_attempts_cleanup_and_keeps_failure(self):
        r, b = self.run_case("create")
        self.assertEqual(b.calls, ["create", "delete"])
        self.assertFalse(r["execution_completed"])
        self.assertNotIn("secret", json.dumps(r))

    def test_execution_failure_still_deletes(self):
        r, b = self.run_case("first")
        self.assertEqual(b.calls[-1], "delete")
        self.assertNotIn("evaluate", b.calls)
        self.assertFalse(r["replay_complete"])

    def test_cleanup_failure_does_not_mask_official_result(self):
        r, _ = self.run_case("delete")
        self.assertEqual(r["official"]["passed_count"], 1)
        self.assertEqual(r["failures"][0]["stage"], "cleanup")
        self.assertFalse(r["execution_completed"])

    def test_invalid_wait_rejected_before_create(self):
        for wait in [-1, float("nan"), float("inf"), True]:
            p = plan()
            p["rounds"][0]["wait_s"] = wait
            with self.assertRaises(ValueError):
                w.validate(p)

    def test_duplicate_round_and_required_test_rejected(self):
        p = plan()
        p["rounds"][1]["round"] = 1
        with self.assertRaises(ValueError):
            w.validate(p)
        p = plan()
        p["official"]["required_tests"] = ["a", "a"]
        with self.assertRaises(ValueError):
            w.validate(p)

    def test_existing_output_never_overwritten(self):
        clock = Clock()
        backend = Backend(clock)
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(FileExistsError):
                w.run(plan(), backend, tmp)
        self.assertEqual(backend.calls, [])

    def test_internal_error_overrides_parsed_passes(self):
        r = w.classify(dict(stdout="INTERNALERROR> localhost", stderr=""), {"a": "PASSED"}, ["a"])
        self.assertFalse(r["evaluator_healthy"])
        self.assertFalse(r["passed"])

    def test_missing_parser_setup_and_collection_fail_closed(self):
        for parsed, text in [
            (None, ""),
            ({}, ""),
            ({"a": "ERROR"}, ""),
            ({"a": "PASSED"}, "ERROR collecting"),
        ]:
            r = w.classify(dict(stdout=text), parsed, ["a"])
            self.assertEqual(r["result_class"], "evaluator_infrastructure_error")

    def test_empty_hosts_initialized_nonempty_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            hosts = Path(tmp) / "hosts"
            hosts.touch()
            command = w.LOCALHOST_INIT.replace("/etc/hosts", str(hosts))
            subprocess.run(["sh", "-c", command], check=True)
            self.assertIn("127.0.0.1 localhost\n", hosts.read_text())
            hosts.write_text("custom\n")
            subprocess.run(["sh", "-c", command], check=True)
            self.assertEqual(hosts.read_text(), "custom\n")

    def test_optional_agentenv_does_not_import_sdk(self):
        with patch.dict(sys.modules, {"e2b": None}):
            b = build_backend(
                dict(
                    kind="agentenv",
                    endpoint="http://127.0.0.1:18250",
                    template="t",
                    api_key_env="TOKEN",
                    cwd="/app",
                    vm_timeout_s=18000,
                )
            )
            self.assertIsNone(b.sandbox)

    def test_pvisor_argv_does_not_expand_tool_on_host(self):
        b = build_backend(
            dict(
                kind="pvisor",
                binary="/bin/pvisor",
                image="prepared",
                memory="8g",
                vcpus=2,
                cwd="/app",
            )
        )
        with patch("workload_backends.subprocess.run") as run:
            run.return_value = subprocess.CompletedProcess([], 0, "", "")
            b.start()
            self.assertNotIn("--memory-pool", run.call_args_list[0].args[0])
            b.exec("echo $(secret)")
            receipt = b.stop()
            argv = run.call_args_list[1].args[0]
            self.assertEqual(argv[-3:], ["/bin/bash", "-c", "echo $(secret)"])
            self.assertNotIn("shell", run.call_args_list[1].kwargs)
            self.assertTrue(receipt["api_deleted"])
            self.assertFalse(receipt["physical_exit_verified"])

    def test_pvisor_external_pool_is_one_argument_before_image(self):
        config = dict(
            kind="pvisor",
            binary="/bin/pvisor",
            image="prepared",
            memory="8g",
            vcpus=2,
            cwd="/app",
            memory_pool="/tmp/replay pool/socket",
        )
        backend = build_backend(config)
        with patch("workload_backends.subprocess.run") as run:
            run.return_value = subprocess.CompletedProcess([], 0, "", "")
            backend.start()
            argv = run.call_args.args[0]
            index = argv.index("--memory-pool")
            self.assertEqual(argv[index + 1], config["memory_pool"])
            self.assertLess(index, argv.index("prepared"))
            backend.stop()
            self.assertEqual(run.call_count, 2)  # no pool creation or destruction
            self.assertEqual(run.call_args.args[0][-3:], ["rm", "-f", backend.name])

    def test_pool_configuration_rejected_before_execution(self):
        config = dict(
            kind="pvisor",
            binary="/bin/pvisor",
            image="prepared",
            memory="8g",
            vcpus=2,
            cwd="/app",
        )
        with patch("workload_backends.subprocess.run") as run:
            for pool in (None, "", "auto", "relative.sock", 1, "/tmp/\0sock"):
                with self.subTest(pool=pool), self.assertRaises(ValueError):
                    build_backend(dict(config, memory_pool=pool))
            with self.assertRaisesRegex(ValueError, "only supported by pvisor"):
                build_backend(
                    dict(
                        kind="agentenv",
                        endpoint="http://localhost:18250",
                        template="t",
                        api_key_env="TOKEN",
                        cwd="/app",
                        vm_timeout_s=18000,
                        memory_pool="/tmp/pool.sock",
                    )
                )
            run.assert_not_called()

    def test_validate_only_cli_does_not_start_or_import_sdk(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "plan.json").write_text(json.dumps(plan()))
            (root / "backend.json").write_text(
                json.dumps(
                    dict(
                        kind="agentenv",
                        endpoint="http://localhost:18250",
                        template="t",
                        api_key_env="TOKEN",
                        cwd="/app",
                        vm_timeout_s=18000,
                    )
                )
            )
            value = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "replay/workload.py"),
                    "--plan",
                    str(root / "plan.json"),
                    "--backend",
                    str(root / "backend.json"),
                    "--output",
                    str(root / "out"),
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(value.returncode, 0, value.stderr)
            self.assertFalse(json.loads(value.stdout)["started"])
            self.assertFalse((root / "out").exists())


class AgentenvContracts(unittest.TestCase):
    def test_native_sdk_routing_nonzero_exit_and_delete(self):
        import types
        from unittest.mock import MagicMock

        from workload_backends import AgentenvBackend

        class Exit(Exception):
            exit_code = 7
            stdout = "out"
            stderr = "err"

        response = types.SimpleNamespace(
            sandbox_id="owned",
            sandbox_domain="local",
            envd_version="1.0.0",
            envd_access_token="secret",
            traffic_access_token=None,
        )
        api = MagicMock()
        api._create_sandbox.return_value = response
        client = MagicMock()
        client.commands.run.side_effect = Exit()
        sandbox = MagicMock(return_value=client)
        connection = MagicMock()
        modules = {}
        for name in [
            "e2b",
            "e2b.sandbox_sync.sandbox_api",
            "e2b.connection_config",
            "packaging.version",
            "e2b.sandbox.commands.command_handle",
        ]:
            modules[name] = types.ModuleType(name)
        modules["e2b"].Sandbox = sandbox
        modules["e2b.sandbox_sync.sandbox_api"].SandboxApi = api
        modules["e2b.connection_config"].ConnectionConfig = connection
        modules["packaging.version"].Version = lambda x: x
        modules["e2b.sandbox.commands.command_handle"].CommandExitException = Exit
        b = AgentenvBackend(
            dict(
                endpoint="http://localhost:1",
                template="t",
                api_key_env="REPLAY_TEST_TOKEN",
                vm_timeout_s=18000,
                cwd="/app",
            )
        )
        with (
            patch.dict(sys.modules, modules),
            patch.dict("os.environ", {"REPLAY_TEST_TOKEN": "key"}),
            patch("importlib.metadata.version", return_value="2.5.0"),
        ):
            b.start()
            result = b.exec("exit 7")
            receipt = b.stop()
        self.assertEqual(result["return_code"], 7)
        self.assertEqual(client.commands.run.call_args.kwargs["timeout"], 0)
        self.assertEqual(
            api._create_sandbox.call_args.kwargs["lifecycle"],
            {"on_timeout": "kill", "auto_resume": False},
        )
        self.assertEqual(
            connection.call_args.kwargs["extra_sandbox_headers"]["E2b-Sandbox-Id"], "owned"
        )
        client.kill.assert_called_once()
        self.assertTrue(receipt["api_deleted"])
        self.assertFalse(receipt["physical_exit_verified"])

    def test_unknown_creation_cannot_claim_cleanup(self):
        from workload_backends import AgentenvBackend

        with self.assertRaises(RuntimeError):
            AgentenvBackend({}).stop()

    def test_cancellation_is_saved_and_cleanup_attempted(self):
        clock = Clock()
        b = Backend(clock)

        def interrupt(command):
            raise KeyboardInterrupt()

        b.exec = interrupt
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "new"
            with self.assertRaises(KeyboardInterrupt):
                w.run(plan(), b, out, clock=clock, sleep=clock.sleep)
            r = json.loads((out / "result.json").read_text())
            self.assertEqual(r["failures"][0]["type"], "KeyboardInterrupt")
            self.assertFalse(r["execution_completed"])
            self.assertEqual(b.calls[-1], "delete")


if __name__ == "__main__":
    unittest.main()
