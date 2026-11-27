"""PR conversation rendering and routing; no GitHub writes."""

import importlib.util
import io
import json
import os
import unittest
import zipfile
from unittest.mock import Mock, patch

import yaml

from tests.fixtures import ROOT

SPEC = importlib.util.spec_from_file_location("reporter", ROOT / "scripts/ci/vm_guest_comment.py")
reporter = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reporter)
WORKFLOWS = ROOT / ".github/workflows"
PUBLISHER = yaml.safe_load((WORKFLOWS / "vm-guest-comment.yml").read_text())
WRITE_STEP = PUBLISHER["jobs"]["comment"]["steps"][0]
MARKER = "<!-- pvisor-vm-guest-report"


def report(slow=False):
    rows = []
    for metric in ["tmpfs-write", *(f"workload-{i}" for i in range(16))]:
        for backend in reporter.BACKENDS:
            ref = backend == "firecracker"
            fails = slow and (backend, metric) == ("linux", "tmpfs-write")
            rows.append(
                {
                    "condition": f"{backend}-{metric}",
                    "baseline_ns_per_op": 1e6,
                    "candidate_ns_per_op": 1.2e6 if ref or fails else 1e6,
                    "change_pct": 20 if ref or fails else 0,
                    "passed": None if ref else not fails,
                }
            )
    return {"rows": rows, "passed": not slow, "threshold_pct": 15}


def zipped(name, content):
    data = io.BytesIO()
    with zipfile.ZipFile(data, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr(name, content)
    return data.getvalue()


def startup_report():
    names = [
        "linux-direct-command-start",
        "rust-native-direct-command-start",
        "firecracker-direct-command-start",
        "linux-warm-command-start",
    ]
    return {
        "rows": [
            {
                "condition": name,
                "baseline_ns_per_op": 1e6,
                "candidate_ns_per_op": 1.16e6,
                "change_pct": 16.0,
                "passed": None if name == "firecracker-direct-command-start" else False,
            }
            for name in names
        ],
        "threshold_pct": 15,
        "passed": False,
    }


class VmGuestCommentTests(unittest.TestCase):
    def setUp(self):
        self.run = dict(
            id=321,
            run_number=37,
            run_attempt=1,
            pull_requests=[{"number": 7}],
            head_sha="a" * 40,
            head_branch="feature",
            head_repository={"id": 456, "owner": {"login": "fork-owner"}},
            html_url="https://github.com/base/pvisor/actions/runs/321",
        )
        self.event = {"workflow_run": self.run, "repository": {"full_name": "base/pvisor"}}
        self.pull = {
            "number": 7,
            "state": "open",
            "head": {
                "sha": self.run["head_sha"],
                "ref": "feature",
                "repo": {"id": 456},
            },
        }
        self.artifact = {
            "name": "vm-guest-table-1",
            "size_in_bytes": 600,
            "id": 42,
            "expired": False,
        }

    def write_comment(self, table, pages, kind="guest"):
        api = Mock(side_effect=[json.dumps(self.pull), json.dumps(pages), "{}"])
        env = {
            "GITHUB_REPOSITORY": "base/pvisor",
            "GITHUB_SERVER_URL": "https://github.com",
            "BENCH_RUN": "321",
            "RUN_NUMBER": "37",
            "ATTEMPT": "1",
            "KIND": kind,
            "PR": "7",
            "HEAD_SHA": self.run["head_sha"],
            "TABLE": json.dumps(table),
        }
        with patch.dict(os.environ, env), patch("subprocess.check_output", api):
            try:
                exec(WRITE_STEP["run"], {})
            except SystemExit as exit:
                self.assertEqual(exit.code, 0)
        return api

    def test_same_repository_pr_calls_publisher_even_after_failed_gate(self):
        benchmark = yaml.safe_load((WORKFLOWS / "pvisor-benchmark.yml").read_text())
        job = benchmark["jobs"]["pr-report"]
        self.assertEqual(job["uses"], "./.github/workflows/vm-guest-comment.yml")
        self.assertEqual(job["needs"], "vm-guest")
        self.assertIn("!cancelled()", job["if"])
        self.assertIn(
            "github.event.pull_request.head.repo.id == github.event.repository.id", job["if"]
        )
        self.assertIn("workflow_call", PUBLISHER[True])
        self.assertEqual(PUBLISHER["jobs"]["prepare"]["permissions"]["pull-requests"], "read")
        self.assertEqual(PUBLISHER["jobs"]["comment"]["permissions"], {"pull-requests": "write"})
        self.assertEqual(len(PUBLISHER["jobs"]["comment"]["steps"]), 1)
        self.assertEqual(WRITE_STEP["shell"], "python")

    @patch.object(reporter, "comparison")
    def test_direct_pr_reads_its_own_run_and_attempt(self, comparison):
        comparison.side_effect = ["guest table", "startup table"]
        event = {"repository": {"id": 456, "full_name": "base/pvisor"}, "pull_request": self.pull}
        outputs = reporter.prepare(event, {"GITHUB_RUN_ID": "321", "GITHUB_RUN_ATTEMPT": "2"})
        self.assertEqual(
            outputs, {"pr": "7", "guest": '"guest table"', "startup": '"startup table"'}
        )
        self.assertEqual(
            comparison.call_args.args,
            ("repos/base/pvisor", {"id": 321, "run_attempt": 2}, "startup"),
        )
        event["repository"]["id"] = 789
        with self.assertRaisesRegex(ValueError, "fork reports require"):
            reporter.prepare(event, {})

    def test_table_includes_all_cells_and_highlights_only_product_regressions(self):
        for slow in (False, True):
            text = reporter.render(report(slow))
            self.assertIn("**FAIL** · 1/34" if slow else "**PASS** · 0/34", text)
            self.assertEqual(text.count("\n| `"), 17)
            self.assertEqual(text.count("❌"), int(slow))
            self.assertTrue(text.splitlines()[6].endswith("1.000 → 1.200 (+20.00%) |"))

    def test_rejects_incomplete_duplicate_or_untrusted_rows(self):
        for change in ("missing", "duplicate", "markup", "invalid estimate", "wrong verdict"):
            with self.subTest(change=change):
                data = report()
                rows = data["rows"]
                if change == "missing":
                    del rows[0:3]
                elif change == "duplicate":
                    rows.append(rows[0])
                elif change == "markup":
                    rows[0]["condition"] = "linux-@someone"
                elif change == "invalid estimate":
                    rows[0]["baseline_ns_per_op"] = float("nan")
                else:
                    data["passed"] = False
                with self.assertRaises(ValueError):
                    reporter.render(data)

    @patch.object(reporter, "gh")
    def test_fork_updates_only_owned_comment_and_includes_failed_table(self, gh):
        self.run["pull_requests"] = []
        body = f"{MARKER} 36:1 -->\nold"
        pages = [
            [{"id": 123, "user": {"login": "human"}, "body": body}],
            [{"id": 124, "user": {"login": "github-actions[bot]"}, "body": body}],
        ]
        gh.side_effect = [
            json.dumps([self.pull]),
            json.dumps({"artifacts": [self.artifact]}),
            zipped("comparison.json", json.dumps(report(slow=True))),
            '{"artifacts":[]}',
        ]
        outputs = reporter.prepare(self.event, {})
        api = self.write_comment(json.loads(outputs["guest"]), pages)
        self.assertIn("head=fork-owner%3Afeature", gh.call_args_list[0].args[0])
        self.assertEqual(
            api.call_args.args[0][2:5],
            ["repos/base/pvisor/issues/comments/124", "--method", "PATCH"],
        )
        body = json.loads(api.call_args.kwargs["input"])["body"]
        self.assertIn("**FAIL** · 1/34", body)
        self.assertIn("actions/runs/321/attempts/1", body)

    @patch.object(reporter, "gh")
    def test_first_run_without_comparison_posts_incomplete_not_pass(self, gh):
        gh.side_effect = [json.dumps(self.pull), '{"artifacts":[]}', '{"artifacts":[]}']
        outputs = reporter.prepare(self.event, {})
        api = self.write_comment(json.loads(outputs["startup"]), [[]], "startup")
        self.assertEqual(
            api.call_args.args[0][2:5], ["repos/base/pvisor/issues/7/comments", "--method", "POST"]
        )
        self.assertIn("No complete comparison", json.loads(api.call_args.kwargs["input"])["body"])

    def test_never_posts_for_stale_head_or_supersedes_newer_attempt(self):
        self.pull["head"]["sha"] = "b" * 40
        self.assertEqual(self.write_comment("guest", [[]]).call_count, 1)
        self.pull["head"]["sha"] = self.run["head_sha"]
        newer = {"user": {"login": "github-actions[bot]"}, "body": f"{MARKER} 37:2 -->\n"}
        self.assertEqual(self.write_comment("guest", [[newer]]).call_count, 2)

    @patch.object(reporter, "gh")
    def test_rejects_unexpected_archive_without_rendering_its_content(self, gh):
        gh.side_effect = [
            json.dumps(self.pull),
            json.dumps({"artifacts": [self.artifact]}),
            zipped("../comparison.json", "@someone"),
            '{"artifacts":[]}',
        ]
        table = json.loads(reporter.prepare(self.event, {})["guest"])
        self.assertIn("Invalid comparison artifact", table)
        self.assertNotIn("@someone", table)

    @patch.object(reporter, "gh")
    def test_startup_owns_a_two_row_table_and_does_not_overwrite_guest_comment(self, gh):
        existing = {
            "id": 123,
            "user": {"login": "github-actions[bot]"},
            "body": f"{MARKER} 37:1 -->\n## VM guest performance",
        }
        artifact = {**self.artifact, "name": "vm-startup-table-1"}
        gh.side_effect = [
            json.dumps(self.pull),
            '{"artifacts":[]}',
            json.dumps({"artifacts": [artifact]}),
            zipped("comparison.json", json.dumps(startup_report())),
        ]
        outputs = reporter.prepare(self.event, {})
        api = self.write_comment(json.loads(outputs["startup"]), [[existing]], "startup")
        self.assertIn("?name=vm-startup-table-1", gh.call_args_list[2].args[0])
        self.assertIn("POST", api.call_args.args[0])
        body = json.loads(api.call_args.kwargs["input"])["body"]
        self.assertIn("## VM startup performance", body)
        self.assertIn("**FAIL** · 3/3", body)
        self.assertEqual(body.count("N/A:"), 2)
        self.assertEqual(body.count("❌"), 3)
        self.assertIn("Create → guest entrypoint starts (confirmed pool hit)", body)
        self.assertNotIn("first successful command", body)
        self.assertEqual(len([line for line in body.splitlines() if line.startswith("|")]), 4)

    def test_startup_rejects_missing_warm_hit_or_unsupportable_measurement(self):
        for missing in (
            "linux-direct-command-start",
            "linux-warm-command-start",
            "firecracker-direct-command-start",
        ):
            data = startup_report()
            data["rows"] = [r for r in data["rows"] if r["condition"] != missing]
            with self.subTest(missing=missing), self.assertRaises(ValueError):
                reporter.render(data, startup=True)
        data = startup_report()
        data["rows"].append({**data["rows"][0], "condition": "rust-native-daemon-hit"})
        with self.assertRaises(ValueError):
            reporter.render(data, startup=True)

    def test_running_state_comparison_cannot_be_relabelled_guest_command_start(self):
        data = startup_report()
        for row in data["rows"]:
            row["condition"] = (
                row["condition"]
                .replace("direct-command-start", "direct-running")
                .replace("warm-command-start", "create-running")
            )
        with self.assertRaises(ValueError):
            reporter.render(data, startup=True)

    @patch.object(reporter, "gh")
    def test_rerunning_other_jobs_preserves_original_table(self, gh):
        self.run["run_attempt"] = 2
        gh.side_effect = [
            json.dumps(self.pull),
            '{"artifacts":[]}',
            '{"jobs":[{"name":"Process and Run Bundle report"}]}',
            '{"artifacts":[]}',
            '{"jobs":[{"name":"Process and Run Bundle report"}]}',
        ]
        self.assertEqual(reporter.prepare(self.event, {}), {"pr": "7"})
