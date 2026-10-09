import copy
import io
import sys
import unittest
from contextlib import ExitStack, redirect_stderr
from unittest import mock

from apply_plan_ab import APPLY, main, validate_sources


def fixture():
    cli = dict(
        rustc=1,
        features='["default", "gateway"]',
        declared_features='["default", "gateway"]',
        profile=2,
        rustflags=[],
        config=3,
        compile_kind=0,
    )
    units = dict(cli=cli, libraries=[cli | dict(name="tokio", features='["default", "time"]')])
    left = dict(
        rustc="same compiler",
        cargo="same cargo",
        command=[
            "cargo",
            "build",
            "--release",
            "--locked",
            "--offline",
            "-p",
            "pvisor",
            "--bin",
            "pvisor",
            "--features",
            "gateway",
            "--target-dir",
            "/old/target",
        ],
        compiled_units=units,
        pvisor_sha256="old",
    )
    right = copy.deepcopy(left) | dict(pvisor_sha256="new")
    before = [
        dict(path=APPLY, sha256="old"),
        dict(path="crates/pvisor/src/lib.rs", sha256="unchanged"),
        dict(path="Cargo.lock", sha256="unchanged"),
    ]
    after = copy.deepcopy(before)
    after[0]["sha256"] = "new"
    return left, right, before, after


class ApplyPlanAbTests(unittest.TestCase):
    def test_apply_comparison_accepts_only_targeted_compiled_source_delta(self):
        assert validate_sources(*fixture()) == [APPLY]

    def test_unmatched_apply_binaries_cannot_establish_optimization_gain(self):
        for mutation in [
            "compiler",
            "dependency",
            "unrelated-source",
            "inventory",
            "identical",
            "no-apply",
            "debug",
        ]:
            with self.subTest(mutation=mutation):
                left, right, before, after = fixture()
                if mutation == "compiler":
                    right["rustc"] = "different compiler"
                elif mutation == "dependency":
                    after[2]["sha256"] = "changed dependency"
                elif mutation == "unrelated-source":
                    after[1]["sha256"] = "changed unrelated source"
                elif mutation == "inventory":
                    after.pop()
                elif mutation == "identical":
                    right["pvisor_sha256"] = "old"
                elif mutation == "no-apply":
                    after[0]["sha256"] = "old"
                else:
                    right["command"].remove("--release")
                with self.assertRaises(ValueError):
                    validate_sources(left, right, before, after)

    def test_output_directory_and_equivalent_feature_namespace_do_not_change_build(self):
        left, right, before, after = fixture()
        right["command"][-1] = "/new/target"
        right["command"][right["command"].index("gateway")] = "pvisor/gateway"
        assert validate_sources(left, right, before, after)

    def test_joint_targets_cannot_establish_apply_gain(self):
        for extra in [
            ["--example", "unrelated_example"],
            ["-p", "unrelated-package"],
            ["--all-targets"],
            ["--tests"],
            ["--lib"],
        ]:
            with self.subTest(extra=extra):
                left, right, before, after = fixture()
                right["command"].extend(extra)
                with self.assertRaises(ValueError):
                    validate_sources(left, right, before, after)

    def test_actual_build_configuration_must_match(self):
        for mutation in [
            "missing",
            "library-features",
            "profile",
            "rustflags",
            "command",
            "duplicate-source",
        ]:
            with self.subTest(mutation=mutation):
                left, right, before, after = fixture()
                if mutation == "missing":
                    right.pop("compiled_units")
                elif mutation == "library-features":
                    right["compiled_units"]["libraries"][0]["features"] = '["test-util"]'
                elif mutation == "profile":
                    right["compiled_units"]["cli"]["profile"] = 99
                elif mutation == "rustflags":
                    right["compiled_units"]["cli"]["rustflags"] = ["-Ctarget-cpu=native"]
                elif mutation == "command":
                    right["command"].extend(["--target", "other-target"])
                else:
                    after.append(copy.deepcopy(after[0]))
                with self.assertRaises(ValueError):
                    validate_sources(left, right, before, after)

    def test_tracing_requires_diagnostic_mode_and_complete_provenance(self):
        for options in [
            ["--trace-syscalls", "tracer"],
            ["--tracer-receipt", "receipt"],
            ["--trace-syscalls", "tracer", "--tracer-receipt", "receipt"],
            ["--profile", "--trace-syscalls", "tracer"],
            ["--profile", "--tracer-receipt", "receipt"],
        ]:
            with self.subTest(options=options):
                with ExitStack() as resources:
                    captured_stderr = io.StringIO()
                    resources.enter_context(redirect_stderr(captured_stderr))
                    argv = [
                        "apply_plan_ab.py",
                        "--baseline",
                        "old",
                        "--candidate",
                        "new",
                        "--baseline-receipt",
                        "old.json",
                        "--candidate-receipt",
                        "new.json",
                        "--firmware",
                        "firmware",
                        "--output",
                        "new-output",
                        *options,
                    ]
                    resources.enter_context(mock.patch.object(sys, "argv", argv))
                    with self.assertRaises(SystemExit) as error:
                        main()
                    assert error.exception.code == 2
                    assert (
                        "syscall tracing requires --profile and both tracer binary/receipt"
                        in captured_stderr.getvalue()
                    )
