"""Contract tests for scenario resources and native Scrut integration."""

import ctypes
import errno
import hashlib
import importlib.util
import json
import os
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

SPEC = Path(__file__).resolve().parents[2]
FIXTURE = SPEC / "fixture.py"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ProcessOwnershipTests(unittest.TestCase):
    def setUp(self):
        self.fixture = load("ownership_fixture", FIXTURE)
        self.process = mock.Mock(pid=32123, returncode=None)

    def test_deadline_rechecks_exit_after_observer_is_descheduled(self):
        fixture = self.fixture
        for exited in (True, False):
            with (
                self.subTest(exited=exited),
                mock.patch.object(fixture, "child_exited", side_effect=[False, exited]),
                mock.patch.object(fixture.time, "monotonic", side_effect=[0, 0, 1]),
                mock.patch.object(fixture.time, "sleep"),
            ):
                self.assertEqual(fixture.wait_for_child_exit(self.process, 0.2, 123, 456), exited)
                self.process.wait.assert_not_called()

    def test_darwin_waitid_uses_native_abi_without_reaping(self):
        fixture = self.fixture
        info_type = fixture.DarwinSiginfo
        self.assertEqual(ctypes.sizeof(info_type), 104)
        self.assertEqual(info_type.pid.offset, 12)
        self.assertEqual(info_type.status.offset, 20)
        self.assertEqual(info_type.reserved.offset, 48)
        for pid in (0, self.process.pid):
            with self.subTest(pid=pid):

                def observe(kind, child, pointer, flags):
                    self.assertEqual((kind, child, flags), (1, self.process.pid, 0x25))
                    self.assertIsInstance(pointer._obj, info_type)
                    pointer._obj.pid = pid
                    return 0

                waitid = mock.Mock(side_effect=observe)
                with (
                    mock.patch.object(fixture.sys, "platform", "darwin"),
                    mock.patch.object(
                        fixture.ctypes, "CDLL", return_value=mock.Mock(waitid=waitid)
                    ),
                ):
                    self.assertEqual(fixture.child_exited(self.process), pid != 0)
                self.assertEqual(
                    waitid.argtypes,
                    [ctypes.c_int, ctypes.c_uint, ctypes.POINTER(info_type), ctypes.c_int],
                )
                self.process.wait.assert_not_called()

    def test_darwin_waitid_errors_do_not_become_an_exit_observation(self):
        fixture = self.fixture
        with (
            mock.patch.object(fixture.sys, "platform", "darwin"),
            mock.patch.object(
                fixture.ctypes, "CDLL", return_value=mock.Mock(waitid=mock.Mock(return_value=-1))
            ),
            mock.patch.object(fixture.ctypes, "get_errno", return_value=errno.ECHILD),
            self.assertRaises(ChildProcessError),
        ):
            fixture.child_exited(self.process)
        self.process.wait.assert_not_called()

    def test_darwin_zombie_only_eperm_keeps_anchor_until_last_signal(self):
        fixture = self.fixture
        events = []

        def killpg(pid, signum):
            self.assertEqual(pid, self.process.pid)
            self.assertIsNone(self.process.returncode)
            events.append(signum)
            if signum == signal.SIGKILL:
                raise PermissionError(errno.EPERM, "zombie-only group")

        def reap(*, timeout):
            self.assertEqual(events, [signal.SIGTERM, signal.SIGKILL])
            self.assertEqual(timeout, 3)
            self.process.returncode = -signal.SIGTERM

        self.process.wait.side_effect = reap
        with (
            mock.patch.object(fixture.sys, "platform", "darwin"),
            mock.patch.object(fixture, "child_exited", return_value=True),
            mock.patch.object(fixture.os, "killpg", side_effect=killpg),
            mock.patch.object(fixture.time, "sleep"),
            mock.patch.object(
                fixture.subprocess,
                "run",
                return_value=subprocess.CompletedProcess([], 0, "32123 32123 Z\n32124 32123 Z+\n"),
            ) as query,
        ):
            fixture.stop_group(self.process)
        args, kwargs = query.call_args
        self.assertEqual(args[0], ["ps", "-g", "32123", "-o", "pid=,pgid=,stat="])
        self.assertEqual(kwargs["env"]["COMMAND_MODE"], "unix2003")
        self.assertEqual(kwargs["stdin"], subprocess.DEVNULL)
        self.assertEqual(kwargs["timeout"], 2)
        self.assertTrue(kwargs["check"])
        self.process.wait.assert_called_once_with(timeout=3)

    def test_eperm_requires_exited_owned_anchor_and_no_live_group_members(self):
        fixture = self.fixture
        for output in (
            "",
            "32123 32123 R\n",
            "32123 32123 Z\n32124 32123 S\n",
            "32123 32123 Z\n32124 32123 T\n",
            "32124 32123 Z\n",
            "32123 99999 Z\n",
            "unparseable\n",
        ):
            with (
                self.subTest(output=output),
                mock.patch.object(fixture.sys, "platform", "darwin"),
                mock.patch.object(fixture, "child_exited", return_value=True),
                mock.patch.object(
                    fixture.os, "killpg", side_effect=PermissionError(errno.EPERM, "denied")
                ),
                mock.patch.object(
                    fixture.subprocess,
                    "run",
                    return_value=subprocess.CompletedProcess([], 0, output),
                ),
                self.assertRaises(PermissionError),
            ):
                fixture.signal_owned_group(self.process, signal.SIGKILL)
        for platform, exited in (("darwin", False), ("linux", True)):
            with (
                self.subTest(platform=platform, exited=exited),
                mock.patch.object(fixture.sys, "platform", platform),
                mock.patch.object(fixture, "child_exited", return_value=exited),
                mock.patch.object(
                    fixture.os, "killpg", side_effect=PermissionError(errno.EPERM, "denied")
                ),
                mock.patch.object(fixture.subprocess, "run") as query,
                self.assertRaises(PermissionError),
            ):
                fixture.signal_owned_group(self.process, signal.SIGKILL)
            query.assert_not_called()
        self.process.wait.assert_not_called()

    def test_group_query_failure_is_a_hard_cleanup_failure(self):
        fixture = self.fixture
        for error in (
            subprocess.CalledProcessError(1, "ps"),
            subprocess.TimeoutExpired("ps", 2),
        ):
            with (
                self.subTest(error=error),
                mock.patch.object(fixture.sys, "platform", "darwin"),
                mock.patch.object(fixture, "child_exited", return_value=True),
                mock.patch.object(
                    fixture.os, "killpg", side_effect=PermissionError(errno.EPERM, "denied")
                ),
                mock.patch.object(fixture.subprocess, "run", side_effect=error),
                self.assertRaises(type(error)),
            ):
                fixture.signal_owned_group(self.process, signal.SIGTERM)
        self.process.wait.assert_not_called()

    def test_already_reaped_anchor_is_never_signalled(self):
        fixture = self.fixture
        self.process.returncode = 0
        with mock.patch.object(fixture.os, "killpg") as killpg, self.assertRaises(RuntimeError):
            fixture.stop_group(self.process)
        killpg.assert_not_called()


class FixtureTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.env = {**os.environ, "PVISOR_CASE_ARTIFACTS": str(self.root / "artifacts")}
        self.env.pop("PVISOR_CASE_REQUIRE_PASS", None)
        self.env.pop("PVISOR_CASE_KEEP", None)

    def run_case(self, script, *args, env=None):
        return subprocess.run(
            [sys.executable, str(FIXTURE), "none", "S-FIXTURE-001", *args],
            input=script,
            text=True,
            capture_output=True,
            env=env or self.env,
            timeout=10,
        )

    def assert_dead(self, pid):
        status = subprocess.run(
            ["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True
        ).stdout.strip()
        self.assertTrue(
            not status or status.startswith("Z"), f"live owned process: {pid}: {status}"
        )

    def test_strict_shell_and_exact_stream_logs(self):
        for script in (
            "false; echo swallowed",
            "false | true",
            'echo "$UNDEFINED_FIXTURE_VARIABLE"',
        ):
            with self.subTest(script=script):
                result = self.run_case(script)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("FAIL S-FIXTURE-001", result.stdout)
        result = self.run_case(
            "printf 'out\\000bytes'; printf 'err\\377bytes' >&2",
            env={**self.env, "PVISOR_CASE_KEEP": "1"},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        roots = sorted(
            (self.root / "artifacts").iterdir(), key=lambda path: path.stat().st_mtime_ns
        )
        self.assertEqual((roots[-1] / "stdout").read_bytes(), b"out\x00bytes")
        self.assertEqual((roots[-1] / "stderr").read_bytes(), b"err\xffbytes")

    def test_independent_workspace_home_xdg_and_session_state(self):
        script = """
test ! -e marker
test ! -e "$HOME/marker"
test ! -e "$XDG_DATA_HOME/marker"
test ! -e "$PVISOR_RUN_HOME/marker"
touch marker "$HOME/marker" "$XDG_DATA_HOME/marker" "$PVISOR_RUN_HOME/marker"
test "$PWD" = "$WS"
"""
        for _ in range(2):
            result = self.run_case(script, env={**self.env, "PVISOR_CASE_KEEP": "1"})
            self.assertEqual(result.returncode, 0, result.stderr)
        roots = list((self.root / "artifacts").iterdir())
        self.assertEqual(len(roots), 2)
        self.assertNotEqual(roots[0], roots[1])

    def test_explicit_container_and_image_store_overrides_cannot_escape_case(self):
        foreign = self.root / "foreign"
        foreign.mkdir()
        marker = foreign / "marker"
        marker.write_bytes(b"foreign bytes\x00\xff")
        environment = {
            **self.env,
            "PVISOR_DOCKER_HOME": str(foreign),
            "PVISOR_IMAGE_STORE": str(foreign),
            "PVISOR_CASE_KEEP": "1",
        }
        script = """
test "$PVISOR_DOCKER_HOME" = "$CASE_ROOT/docker"
test "$PVISOR_IMAGE_STORE" = "$CASE_ROOT/images"
test ! -e "$PVISOR_DOCKER_HOME"
test ! -e "$PVISOR_IMAGE_STORE"
mkdir "$PVISOR_DOCKER_HOME" "$PVISOR_IMAGE_STORE"
printf owned > "$PVISOR_DOCKER_HOME/marker"
printf image > "$PVISOR_IMAGE_STORE/marker"
"""
        for _ in range(2):
            result = self.run_case(script, env=environment)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        roots = list((self.root / "artifacts").iterdir())
        self.assertEqual(len(roots), 2)
        for root in roots:
            self.assertEqual((root / "docker/marker").read_bytes(), b"owned")
            self.assertEqual((root / "images/marker").read_bytes(), b"image")
        self.assertEqual(list(foreign.iterdir()), [marker])
        self.assertEqual(marker.read_bytes(), b"foreign bytes\x00\xff")

    def test_normal_failure_and_timeout_clean_background_children(self):
        for ending in ("true", "exit 1", "wait"):
            with self.subTest(ending=ending):
                result = self.run_case(
                    f'sleep 30 >/dev/null 2>&1 & echo $! > "$CASE_ROOT/pid"; {ending}',
                    "--timeout",
                    "0.3",
                    env={**self.env, "PVISOR_CASE_KEEP": "1"},
                )
                roots = sorted(
                    (self.root / "artifacts").iterdir(), key=lambda path: path.stat().st_mtime_ns
                )
                self.assert_dead(int((roots[-1] / "pid").read_text()))
                if ending == "wait":
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("TIMEOUT", result.stdout)

    def test_sigterm_cleans_descendants(self):
        script = self.root / "stdin"
        script.write_text('sleep 30 >/dev/null 2>&1 & echo $! > "$CASE_ROOT/pid"; wait\n')
        with script.open() as stdin:
            process = subprocess.Popen(
                [sys.executable, str(FIXTURE), "none", "S-FIXTURE-001"],
                stdin=stdin,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=self.env,
            )
        try:
            deadline = time.monotonic() + 5
            while not list((self.root / "artifacts").glob("*/pid")):
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.02)
            pidfile = next((self.root / "artifacts").glob("*/pid"))
            process.send_signal(signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 143, stderr)
            self.assertIn(b"INTERRUPTED", stdout)
            self.assert_dead(int(pidfile.read_text()))
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()

    def test_xfail_is_not_an_expected_exit_code(self):
        platform = "macos" if sys.platform == "darwin" else "linux"
        flags = ("--xfail-platform", platform, "--xfail-reason", "fixture known violation")
        for script, verdict, success in (
            ("exit 1", "XFAIL", True),
            ("true", "XPASS", False),
            ("exit 77", "SKIP", False),
            ("exit 126", "FAIL", False),
            ("exit 127", "FAIL", False),
            ("kill -TERM $$", "FAIL", False),
            ("sleep 30", "TIMEOUT", False),
        ):
            with self.subTest(script=script):
                result = self.run_case(script, *flags, "--timeout", "0.2")
                self.assertEqual(result.returncode == 0, success, result.stderr)
                self.assertIn(verdict + " S-FIXTURE-001", result.stdout)
                roots = sorted(
                    (self.root / "artifacts").iterdir(), key=lambda path: path.stat().st_mtime_ns
                )
                self.assertEqual(
                    json.loads((roots[-1] / "result.json").read_text())["verdict"], verdict
                )
        result = self.run_case("exit 1", *flags, env={**self.env, "PVISOR_CASE_REQUIRE_PASS": "1"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("XFAIL", result.stdout)

    def test_use_xfail_is_strict_even_without_suite_environment(self):
        platform = "macos" if sys.platform == "darwin" else "linux"
        result = subprocess.run(
            [
                sys.executable,
                str(FIXTURE),
                "none",
                "S-USE-999",
                "--xfail-platform",
                platform,
                "--xfail-reason",
                "fixture violation",
            ],
            input="exit 1",
            text=True,
            capture_output=True,
            env=self.env,
            timeout=10,
        )
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("XFAIL S-USE-999", result.stdout)

    @unittest.skipUnless(sys.platform == "linux", "Linux subreaper ownership")
    def test_detached_and_double_forked_children_are_cleaned(self):
        for ending in ("true", "exit 1", "wait"):
            with self.subTest(ending=ending):
                script = f"""
{shlex.quote(sys.executable)} - <<'PY'
import os
import time
from pathlib import Path
if os.fork() == 0:
    os.setsid()
    if os.fork() == 0:
        Path(os.environ["CASE_ROOT"], "detached-pid").write_text(str(os.getpid()))
        time.sleep(30)
    time.sleep(30)
    os._exit(0)
os._exit(0)
PY
for ((i=0; i<100; i++)); do
    test ! -f "$CASE_ROOT/detached-pid" || break
    sleep 0.01
done
test -s "$CASE_ROOT/detached-pid"
sleep 30 >/dev/null 2>&1 &
{ending}
"""
                result = self.run_case(
                    script,
                    "--timeout",
                    "0.5",
                    env={**self.env, "PVISOR_CASE_KEEP": "1"},
                )
                roots = sorted(
                    (self.root / "artifacts").iterdir(), key=lambda path: path.stat().st_mtime_ns
                )
                self.assert_dead(int((roots[-1] / "detached-pid").read_text()))
                self.assertEqual(result.returncode == 0, ending == "true", result.stderr)
                if ending == "wait":
                    self.assertIn("TIMEOUT", result.stdout)

    def test_native_scrut_death_cleans_owned_workload(self):
        document = self.root / "cancel.md"
        document.write_text(
            "```scrut {output_stream: combined, timeout: 40s}\n"
            f'$ "{sys.executable}" "{FIXTURE}" none S-FIXTURE-001 <<\'CASE\'\n'
            '> sleep 30 >/dev/null 2>&1 & echo $! > "$CASE_ROOT/pid"; wait\n'
            "> CASE\nPASS S-FIXTURE-001\n```\n"
        )
        process = subprocess.Popen(
            [os.environ["SCRUT_BIN"], "test", str(document)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=self.env,
        )
        try:
            deadline = time.monotonic() + 5
            while not list((self.root / "artifacts").glob("*/pid")):
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.02)
            pidfile = next((self.root / "artifacts").glob("*/pid"))
            process.terminate()
            process.communicate(timeout=10)
            self.assertNotEqual(process.returncode, 0)
            pid = int(pidfile.read_text())
            while True:
                state = subprocess.run(
                    ["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True
                ).stdout.strip()
                if not state or state.startswith("Z"):
                    break
                self.assertLess(time.monotonic(), deadline, f"live owned process: {pid}")
                time.sleep(0.02)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()

    def test_two_native_cells_keep_resources_and_child_shell_state_independent(self):
        document = self.root / "isolation.md"
        cells = []
        for number in (1, 2):
            identifier = f"S-FIXTURE-{number:03d}"
            cells.append(
                "```scrut {output_stream: combined}\n"
                f'$ "{sys.executable}" "{FIXTURE}" none {identifier} <<\'CASE\'\n'
                "> test ! -e marker\n"
                '> test ! -e "$HOME/marker"\n'
                '> test ! -e "$PVISOR_RUN_HOME/marker"\n'
                '> test "${CHILD_STATE-unset}" = unset\n'
                '> touch marker "$HOME/marker" "$PVISOR_RUN_HOME/marker"\n'
                "> export CHILD_STATE=must-not-leak\n"
                f"> CASE\nPASS {identifier}\n```\n"
            )
        document.write_text("\n".join(cells))
        result = subprocess.run(
            [os.environ["SCRUT_BIN"], "test", "--renderer", "json", str(document)],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len(json.loads(result.stdout)), 2)

    def test_real_scrut_matches_streams_and_fails_on_missing_prerequisites(self):
        binary = os.environ["SCRUT_BIN"]
        for script, expected, status in (
            ("true", "PASS S-FIXTURE-001", 0),
            ("exit 77", "PASS S-FIXTURE-001", 50),
            ("false; true", "PASS S-FIXTURE-001", 50),
        ):
            with self.subTest(script=script):
                document = self.root / "native.md"
                document.write_text(
                    f"```scrut {{output_stream: combined, timeout: 5s}}\n"
                    f"$ {sys.executable} \"{FIXTURE}\" none S-FIXTURE-001 <<'CASE'\n"
                    f"> {script}\n> CASE\n{expected}\n```\n"
                )
                result = subprocess.run(
                    [binary, "test", "--renderer", "json", str(document)],
                    env=self.env,
                    capture_output=True,
                    text=True,
                    timeout=10,
                )
                self.assertEqual(result.returncode, status, result.stdout + result.stderr)
        document.write_text(
            "```scrut {output_stream: combined}\n"
            "$ echo expected; echo unexpected >&2\nexpected\n```\n"
        )
        result = subprocess.run(
            [binary, "test", str(document)], capture_output=True, text=True, timeout=10
        )
        self.assertEqual(result.returncode, 50)


class InventoryTests(unittest.TestCase):
    def test_migration_preserves_original_claims_scripts_and_xfail(self):
        # Scrut, not a second Markdown parser, extracts the executable cells.
        # An always-failing shell makes native JSON include each parsed testcase
        # without executing any product command or interpreting fixture code.
        result = subprocess.run(
            [
                os.environ["SCRUT_BIN"],
                "test",
                "--shell",
                shutil.which("false"),
                "--renderer",
                "json",
                str(SPEC),
            ],
            capture_output=True,
            text=True,
            timeout=20,
        )
        self.assertEqual(result.returncode, 50, result.stderr)
        expected = json.loads((Path(__file__).parent / "migration_contracts.json").read_text())
        actual = {}
        for row in json.loads(result.stdout):
            path = Path(row["location"])
            identifier = path.stem
            expression = row["testcase"]["shell_expression"]
            command, separator, body = expression.partition("<<'CASE'\n")
            self.assertTrue(separator and body.endswith("\nCASE"), path)
            invocation = shlex.split(command)
            self.assertTrue(invocation[1].endswith("/fixture.py"), path)
            self.assertEqual(invocation[3], identifier, path)
            arguments = invocation[4:]
            self.assertEqual(len(arguments) % 2, 0)
            options = dict(zip(arguments[::2], arguments[1::2]))
            if identifier not in expected:
                continue

            # These are explicit product metadata paragraphs, not
            # Markdown discovery or fence parsing. Preserve the old guard's
            # exact field extraction and normalized-script hash contract.
            def field(label):
                values = [
                    paragraph.strip()[len(label) :].lstrip(":：").strip()
                    for paragraph in path.read_text().split("\n\n")
                    if paragraph.strip().startswith(label)
                ]
                self.assertEqual(len(values), 1, path)
                return values[0]

            normalized = (
                "\n".join(line.rstrip() for line in body[:-5].splitlines()).rstrip("\n") + "\n"
            )
            fields = [
                identifier,
                field("**语义**"),
                field("**违反示例**"),
                normalized,
                options.get("--xfail-platform", ""),
                options.get("--xfail-reason", ""),
            ]
            actual[identifier] = "sha256:" + hashlib.sha256("\0".join(fields).encode()).hexdigest()
        self.assertEqual(len(actual), 72)
        self.assertEqual(actual, expected)

    def test_preserved_original_ids_have_one_native_document_each(self):
        original = {f"S-USE-{number:03d}" for number in range(1, 17)}
        original |= {f"S-STAGE-{number:03d}" for number in range(1, 15)}
        original |= {
            f"S-DOC-{number:03d}" for number in (*range(1, 23), *range(24, 53), 55, *range(57, 63))
        }
        self.assertEqual(len(original), 88)
        paths = list(SPEC.glob("*/S-*.md"))
        identifiers = [path.stem for path in paths]
        self.assertEqual(len(identifiers), len(set(identifiers)))
        self.assertTrue(original <= set(identifiers))
        for path in paths:
            source = path.read_text()
            self.assertEqual(source.count("```scrut "), 1, path)
            self.assertIn(f" {path.stem} ", source)
            self.assertNotIn("<!--", source)

    def test_native_report_requires_exact_selected_file_multiset(self):
        report = load("case_report", SPEC / "report.py")
        paths = [SPEC / "session/S-USE-001.md", SPEC / "session/S-USE-002.md"]
        rows = [{"location": str(path), "result": {"kind": "success"}} for path in paths]
        report.validate(rows, paths)
        for invalid in ([], rows[:1], rows + rows[:1], rows + [{"result": {"kind": "success"}}]):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                report.validate(invalid, paths)
        rows[0]["result"]["kind"] = "skipped"
        with self.assertRaises(ValueError):
            report.validate(rows, paths)

    def test_tree_oracle_preserves_modes_links_and_all_bytes(self):
        tree = load("case_tree", SPEC / "fixtures/tree.py")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            file = root / "file"
            file.write_bytes(b"content\x00\xff")
            (root / "link").symlink_to("file")
            before = tree.tree_state(root)
            file.chmod(0o700)
            self.assertNotEqual(before, tree.tree_state(root))
            self.assertIn('link "link" -> "file"', before)
            file.write_bytes(b"other\x00\xff")
            self.assertNotEqual(before, tree.tree_state(root))


if __name__ == "__main__":
    unittest.main()
