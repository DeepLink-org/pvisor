"""Git patch application baseline for the exact same textual file changes."""

import shutil

from .common import checked


def git_trial(ctx, count, trial):
    patch = "".join(
        f"diff --git a/files/f{i:06d} b/files/f{i:06d}\n--- a/files/f{i:06d}\n+++ b/files/f{i:06d}\n@@ -1 +1 @@\n-old-{i}\n+new-{i}\n"
        for i in range(count)
    )
    root = ctx.fresh(f"git-apply-{count}")
    work = root / "workspace"
    (work / "files").mkdir(parents=True)
    for i in range(count):
        (work / "files" / f"f{i:06d}").write_text(f"old-{i}\n")
    checked(["git", "init", "-q"], cwd=work)
    source = root / "patch.diff"
    source.write_text(patch)
    wall, _, _ = ctx.run(["git", "apply", str(source)], cwd=work, timeout=600)
    assert all((work / "files" / f"f{i:06d}").read_text() == f"new-{i}\n" for i in range(count))
    if trial >= 0:
        ctx.record(dict(suite="apply", workload="git-apply", files=count, backend="native",
                        trial=trial, wall_ms=wall, correctness="passed", logs=str(root)))
    shutil.rmtree(work)


def run(ctx):
    for count in map(int, ctx.args.apply_sizes.split(",")):
        samples = min(ctx.args.samples, 3 if count >= 100000 else 10 if count >= 1000 else 30)
        for trial in range(samples):
            git_trial(ctx, count, trial)
            print(f"git apply {count}: {trial}", flush=True)
