"""Guard CI and publication gates using parsed workflow dependencies."""

import unittest

import yaml

from tests.fixtures import ROOT


def dependencies(job):
    needs = job.get("needs", [])
    return {needs} if isinstance(needs, str) else set(needs)


class CiWorkflowTests(unittest.TestCase):
    def test_every_ci_job_is_required_by_the_aggregate(self):
        jobs = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())["jobs"]
        gate = jobs["ci"]
        self.assertEqual(gate["if"], "always()")
        self.assertEqual(set(gate["needs"]), set(jobs) - {"ci"})
        self.assertEqual(len(gate["needs"]), len(set(gate["needs"])))

    def test_publication_consumes_shared_verified_artifacts_after_acceptance(self):
        workflows = ROOT / ".github/workflows"
        shared = yaml.safe_load((workflows / "distribution.yml").read_text())["jobs"]
        self.assertEqual(shared["quality"]["uses"], "./.github/workflows/ci.yml")
        self.assertEqual(shared["wheels"]["uses"], "./.github/workflows/wheel.yml")
        self.assertEqual(dependencies(shared["wheels"]), {"quality"})
        self.assertEqual(dependencies(shared["acceptance"]), {"wheels"})
        self.assertEqual(dependencies(shared["verify"]), {"wheels", "acceptance"})
        for channel, publisher in [("nightly", "publish"), ("release", "github-release")]:
            with self.subTest(channel=channel):
                jobs = yaml.safe_load((workflows / f"{channel}.yml").read_text())["jobs"]
                self.assertEqual(jobs["verify"]["uses"], "./.github/workflows/distribution.yml")
                self.assertIn("verify", dependencies(jobs[publisher]))
                downloads = {
                    step["with"]["name"]
                    for step in jobs[publisher]["steps"]
                    if step.get("uses", "").startswith("actions/download-artifact@")
                }
                self.assertEqual(downloads, {f"{channel}-wheels", f"{channel}-native"})
        release = yaml.safe_load((workflows / "release.yml").read_text())["jobs"]
        self.assertIn("github-release", dependencies(release["pypi-publish"]))
