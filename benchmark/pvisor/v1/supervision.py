"""Machine cost of full-content review, selective application and disposal.

Benchmark: B-SUPERVISION (benchmark/README.md#b-supervision), role user-facing.
Motivation: users need the machine cost of reviewing and keeping selected edits.
Conclusion sought: review/select/apply/drop cost against same-batch Git diff;
human reading and creation/execution of the private task view are excluded.
Design: twenty identical changed files, ten selected, prepared private stage or
Git worktree; full content diff and every final byte checked, randomized rounds.
"""
import random
import shutil
import traceback

from .apply import prepare
from .common import checked


def validate_review(text, count=20):
    for i in range(count):
        if f'f{i:06d}' not in text or f'-old-{i}\n' not in text or f'+new-{i}\n' not in text:
            raise ValueError(f'review omitted original or edited content of file {i}')


def validate_target(work):
    for i in range(20):
        expected=f'{"new" if i<10 else "old"}-{i}\n'
        if (work/'files'/f'f{i:06d}').read_text()!=expected:raise ValueError('selective apply/drop changed the wrong content')


def staged_trial(ctx, trial):
    root,work,stage=prepare(ctx,20,'supervision-stage')
    review_ms,stdout,_=ctx.run([str(ctx.binary),'status','--review','--diff',str(stage)],cwd=work)
    validate_review(stdout);(root/'review.diff').write_text(stdout)
    argv=[str(ctx.binary),'apply',str(stage)]
    for i in range(10):argv+=['--path',f'files/f{i:06d}']
    apply_ms,_,_=ctx.run(argv,cwd=work);validate_target(work)
    drop_ms,_,_=ctx.run([str(ctx.binary),'drop',str(stage)],cwd=work);validate_target(work)
    row=dict(suite='supervision',workload='review-select-10-drop-10',backend='staged',trial=trial,
        review_ms=review_ms,apply_ms=apply_ms,drop_ms=drop_ms,wall_ms=review_ms+apply_ms+drop_ms,
        files_reviewed=20,files_applied=10,files_dropped=10,content_review_complete=True,correctness='passed',logs=str(root))
    if trial>=0:ctx.record(row)
    shutil.rmtree(work);shutil.rmtree(stage/'upper',ignore_errors=True)


def git_trial(ctx, trial):
    root=ctx.fresh('supervision-git');work=root/'workspace';view=root/'view'
    (work/'files').mkdir(parents=True)
    for i in range(20):(work/'files'/f'f{i:06d}').write_text(f'old-{i}\n')
    checked(['git','init','-q'],cwd=work)
    checked(['git','add','files'],cwd=work)
    checked(['git','-c','user.name=benchmark','-c','user.email=benchmark@invalid','commit','-qm','fixture'],cwd=work)
    checked(['git','worktree','add','--detach','-q',str(view),'HEAD'],cwd=work)
    for i in range(20):(view/'files'/f'f{i:06d}').write_text(f'new-{i}\n')
    if any((work/'files'/f'f{i:06d}').read_text()!=f'old-{i}\n' for i in range(20)):raise ValueError('Git private task changed original')
    review_ms,stdout,_=ctx.run(['git','diff','--no-ext-diff','HEAD','--','files'],cwd=view)
    validate_review(stdout);(root/'review.diff').write_text(stdout)
    selection_ms,patch,_=ctx.run(['git','diff','--no-ext-diff','HEAD','--',*[f'files/f{i:06d}' for i in range(10)]],cwd=view)
    path=root/'selected.diff';path.write_text(patch)
    check_ms,_,_=ctx.run(['git','apply','--check',str(path)],cwd=work)
    apply_ms,_,_=ctx.run(['git','apply',str(path)],cwd=work);validate_target(work)
    drop_ms,_,_=ctx.run(['git','worktree','remove','--force',str(view)],cwd=work);validate_target(work)
    row=dict(suite='supervision',workload='review-select-10-drop-10',backend='git-worktree',trial=trial,
        review_ms=review_ms,selection_ms=selection_ms,check_ms=check_ms,apply_ms=apply_ms,drop_ms=drop_ms,
        wall_ms=review_ms+selection_ms+check_ms+apply_ms+drop_ms,files_reviewed=20,files_applied=10,files_dropped=10,
        content_review_complete=True,correctness='passed',logs=str(root))
    if trial>=0:ctx.record(row)
    shutil.rmtree(work)


def run(ctx):
    ctx.metadata['supervision_protocol']=dict(human_participants=0,human_time_measured=False,files=20,selected=10,
        controls='prepared stage and Git worktree with identical contents; private-view creation and task execution excluded',
        review='full original/edited content of every file, not names or JSON metadata',
        timing='sum of complete review, selected patch extraction/check/apply where needed, and disposal command times; fixture/output validation excluded',
        ordering='seeded shuffled backend conditions per paired round',warmups=ctx.args.warmups,samples=ctx.args.samples,
        exclusions='retain all valid slow samples; failed output/content/boundary checks retained separately')
    ctx.save();rng=random.Random(ctx.args.seed)
    for trial in range(-ctx.args.warmups,ctx.args.samples):
        cases=[('staged',staged_trial),('git-worktree',git_trial)];rng.shuffle(cases)
        for backend,run_trial in cases:
            try:run_trial(ctx,trial)
            except Exception as error:
                failure=ctx.capabilities.setdefault('supervision/'+backend,dict(state='failed',failures=[]))
                failure['failures'].append(dict(trial=trial,error=str(error),traceback=traceback.format_exc()));ctx.save()
                if trial<0:raise
