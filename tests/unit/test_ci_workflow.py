"""Guard CI and publication gates using parsed workflow dependencies."""

import unittest

import yaml

from tests.fixtures import ROOT


def dependencies(job):
    needs = job.get("needs", [])
    return {needs} if isinstance(needs, str) else set(needs)


class CiWorkflowTests(unittest.TestCase):
    def test_build_only_candidates_cannot_publish_or_restore_channels(self):
        jobs = yaml.safe_load((ROOT / ".github/workflows/release.yml").read_text())["jobs"]
        select = next(step for step in jobs["validate"]["steps"] if step.get("id") == "mode")
        self.assertIn('[[ "$GITHUB_REF" == refs/heads/main ]]', select["run"])
        for job in ("github-release", "pypi-publish", "homebrew-test", "homebrew-publish"):
            condition = jobs[job]["if"]
            values = {
                "github.event_name": "workflow_dispatch",
                "inputs.channel": "build",
                "needs.validate.result": "success",
                "needs.verify.result": "success",
                "needs.restore.result": "skipped",
                "needs.github-release.result": "success",
                "needs.homebrew-test.result": "success",
                "vars.HOMEBREW_TAP_REPOSITORY": "owner/tap",
            }
            expression = condition.replace("!cancelled()", "True")
            for name, value in values.items():
                expression = expression.replace(name, repr(value))
            expression = expression.replace("&&", " and ").replace("||", " or ")
            with self.subTest(job=job):
                self.assertFalse(eval(expression, {"__builtins__": {}}))
        self.assertEqual(jobs["restore"]["if"], "needs.validate.outputs.recovery == 'true'")

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
        self.assertEqual(dependencies(shared["wheels"]), set())
        self.assertEqual(dependencies(shared["acceptance"]), {"wheels"})
        self.assertEqual(dependencies(shared["verify"]), {"quality", "wheels", "acceptance"})
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
