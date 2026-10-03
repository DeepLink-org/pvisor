import json
import shutil

from .apply import prepare


def run(ctx):
    ctx.metadata["supervision_protocol"] = {
        "human_participants": 0,
        "human_time_measured": False,
        "files": 20,
        "selected": 10,
        "tool_calls": 10,
        "decision_counts": "derived from specified workflows, not observed people",
    }
    for trial in range(ctx.args.samples):
        root, work, stage = prepare(ctx, 20, "supervision")
        wall, stdout, _ = ctx.run(
            [str(ctx.binary), "status", "--review", "--json", str(stage)], cwd=work
        )
        review = json.loads(stdout)
        (root / "review.json").write_text(json.dumps(review, indent=2))
        assert all(f"f{i:06d}" in stdout for i in range(20))
        argv = [str(ctx.binary), "apply", str(stage)]
        for i in range(10):
            argv += ["--path", f"files/f{i:06d}"]
        apply_ms, _, _ = ctx.run(argv, cwd=work)
        assert all((work / "files" / f"f{i:06d}").read_text() == f"new-{i}\n" for i in range(10))
        assert all(
            (work / "files" / f"f{i:06d}").read_text() == f"old-{i}\n" for i in range(10, 20)
        )
        drop_ms, _, _ = ctx.run([str(ctx.binary), "drop", str(stage)], cwd=work)
        assert all(
            (work / "files" / f"f{i:06d}").read_text() == f"old-{i}\n" for i in range(10, 20)
        )
        ctx.record(
            dict(
                suite="supervision",
                workload="review-select-10-drop-10",
                backend="staged",
                trial=trial,
                review_ms=wall,
                apply_ms=apply_ms,
                drop_ms=drop_ms,
                wall_ms=wall + apply_ms + drop_ms,
                files_reviewed=20,
                files_applied=10,
                files_dropped=10,
                correctness="passed",
                logs=str(root),
            )
        )
        shutil.rmtree(work)
        shutil.rmtree(stage / "upper", ignore_errors=True)
