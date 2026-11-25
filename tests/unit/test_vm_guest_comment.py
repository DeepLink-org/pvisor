"""PR conversation rendering and routing; no GitHub writes."""

import importlib.util
import io
import json
import unittest
import zipfile
from unittest.mock import patch

from tests.fixtures import ROOT

SPEC = importlib.util.spec_from_file_location("reporter", ROOT / "scripts/ci/vm_guest_comment.py")
reporter = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reporter)


def report(slow=False):
    rows = []
    for metric in ["tmpfs-write", *(f"workload-{i}" for i in range(14))]:
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

    def test_table_includes_all_cells_and_highlights_only_product_regressions(self):
        for slow in (False, True):
            text = reporter.render(report(slow))
            self.assertIn("**FAIL** · 1/30" if slow else "**PASS** · 0/30", text)
            self.assertEqual(text.count("\n| `"), 15)
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
        body = f"{reporter.MARKER} 36:1 -->\nold"
        pages = [
            [{"id": 123, "user": {"login": "human"}, "body": body}],
            [{"id": 124, "user": {"login": "github-actions[bot]"}, "body": body}],
        ]
        gh.side_effect = [
            json.dumps([self.pull]),
            json.dumps(pages),
            json.dumps({"artifacts": [self.artifact]}),
            zipped("comparison.json", json.dumps(report(slow=True))),
            b"{}",
        ]
        reporter.publish(self.event)
        self.assertIn("head=fork-owner%3Afeature", gh.call_args_list[0].args[0])
        self.assertEqual(
            gh.call_args.args, ("repos/base/pvisor/issues/comments/124", "--method", "PATCH")
        )
        self.assertIn("**FAIL** · 1/30", gh.call_args.kwargs["body"])
        self.assertIn("actions/runs/321/attempts/1", gh.call_args.kwargs["body"])

    @patch.object(reporter, "gh")
    def test_first_run_without_comparison_posts_incomplete_not_pass(self, gh):
        gh.side_effect = [json.dumps(self.pull), "[[]]", '{"artifacts":[]}', b"{}"]
        reporter.publish(self.event)
        self.assertEqual(
            gh.call_args.args, ("repos/base/pvisor/issues/7/comments", "--method", "POST")
        )
        self.assertIn("No complete comparison", gh.call_args.kwargs["body"])

    @patch.object(reporter, "gh")
    def test_never_posts_for_stale_head_or_supersedes_newer_attempt(self, gh):
        self.pull["head"]["sha"] = "b" * 40
        gh.side_effect = [json.dumps(self.pull)]
        reporter.publish(self.event)
        self.assertEqual(gh.call_count, 1)
        self.pull["head"]["sha"] = self.run["head_sha"]
        gh.reset_mock()
        newer = {"user": {"login": "github-actions[bot]"}, "body": f"{reporter.MARKER} 37:2 -->\n"}
        gh.side_effect = [json.dumps(self.pull), json.dumps([[newer]])]
        reporter.publish(self.event)
        self.assertEqual(gh.call_count, 2)

    @patch.object(reporter, "gh")
    def test_rejects_unexpected_archive_without_rendering_its_content(self, gh):
        gh.side_effect = [
            json.dumps(self.pull),
            "[[]]",
            json.dumps({"artifacts": [self.artifact]}),
            zipped("../comparison.json", "@someone"),
            b"{}",
        ]
        reporter.publish(self.event)
        self.assertIn("Invalid comparison artifact", gh.call_args.kwargs["body"])
        self.assertNotIn("@someone", gh.call_args.kwargs["body"])

    @patch.object(reporter, "gh")
    def test_rerunning_other_jobs_preserves_original_table(self, gh):
        self.run["run_attempt"] = 2
        gh.side_effect = [
            json.dumps(self.pull),
            "[[]]",
            '{"artifacts":[]}',
            '{"jobs":[{"name":"Process and Run Bundle report"}]}',
        ]
        reporter.publish(self.event)
        self.assertEqual(gh.call_count, 4)
