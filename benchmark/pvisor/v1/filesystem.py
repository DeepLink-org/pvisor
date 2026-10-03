from pathlib import Path
import hashlib
import json
import random
import shutil

from .common import checked


def fixture(ctx):
    root=ctx.output/'fixtures/filesystem'
    root.mkdir(parents=True)
    tree=root/'tree'
    tree.mkdir()
    data=b'pvisor-fixture-needle '+b'x'*1000+b'\n'
    for i in range(2048):
        directory=tree/f'd{i//64:03d}'
        directory.mkdir(exist_ok=True)
        (directory/f'f{i:05d}.txt').write_bytes(data)
    checked(['git','init','-q'],cwd=tree)
    checked(['git','add','.'],cwd=tree)
    checked(['git','-c','user.name=Benchmark','-c','user.email=benchmark@invalid','commit','-qm','fixture'],cwd=tree)
    payload=b'0123456789abcdef'*(4*1024*1024)
    (root/'payload.bin').write_bytes(payload)
    (root/'rust/src').mkdir(parents=True)
    (root/'rust/Cargo.toml').write_text('[package]\nname="fixture"\nversion="0.1.0"\nedition="2021"\n[workspace]\n')
    declarations=[]
    for i in range(64):
        (root/f'rust/src/m{i}.rs').write_text(f'pub fn value() -> u64 {{ {i} }}\n')
        declarations.append(f'mod m{i};')
    values='+'.join(f'm{i}::value()' for i in range(64))
    (root/'rust/src/main.rs').write_text('\n'.join(declarations)+f'\nfn main() {{ println!("{{}}", {values}); }}\n')
    node=root/'node'
    node.mkdir()
    deps={}
    for i in range(32):
        pkg=node/f'local/p{i}'
        pkg.mkdir(parents=True)
        (pkg/'package.json').write_text(json.dumps(dict(name=f'p{i}',version='1.0.0')))
        for j in range(16):
            (pkg/f'm{j}.js').write_text(f'module.exports = {i+j};\n')
        deps[f'p{i}']=f'file:local/p{i}'
    (node/'package.json').write_text(json.dumps(dict(name='fixture',version='1.0.0',dependencies=deps)))
    (root/'fixture.json').write_text(json.dumps(dict(files=2048,tree_bytes=len(data)*2048,
        payload_bytes=len(payload),payload_sha256=hashlib.sha256(payload).hexdigest(),
        toolchain=str(ctx.toolchain),npm_packages=32)))
    shutil.copy2(Path(__file__).with_name('workload.py'),root/'workload.py')
    return root


def run(ctx):
    source=fixture(ctx)
    rng=random.Random(20261004)
    backends=['native','host','staged','safe','vm']
    if ctx.image:
        backends.extend(['podman','container'])
    modes=['metadata','read','write','git','rg','cargo','npm']
    for mode in modes:
        available=[]
        for backend in backends:
            try:
                one(ctx,source,mode,backend,-1,False)
                available.append(backend)
                ctx.capabilities[f'filesystem/{mode}/{backend}']={'state':'available'}
            except Exception as error:
                ctx.capabilities[f'filesystem/{mode}/{backend}']={'state':'unavailable','reason':str(error)}
            ctx.save()
        for i in range(-ctx.args.warmups,ctx.args.samples):
            order=available.copy()
            rng.shuffle(order)
            for backend in order:
                one(ctx,source,mode,backend,i,i>=0)
            if i>=0 and (i+1)%5==0:
                print(f'filesystem {mode}: {i+1}/{ctx.args.samples}',flush=True)


def one(ctx,source,mode,backend,trial,measure):
    root=ctx.fresh(f'fs-{mode}-{backend}')
    work=root/'workspace'
    checked(['cp','--reflink=auto','-a',str(source),str(work)])
    stage=root/'stage'
    runs=root/'runs'
    home=root/'home'
    home.mkdir()
    env={'PVISOR_RUN_HOME':str(runs),'XDG_CONFIG_HOME':str(root/'config'),'HOME':str(home)}
    payload=['/usr/bin/python3','workload.py',mode]
    command=ctx.command(backend,work,stage,payload)
    wall,stdout,_=ctx.run(command,cwd=work,env=env)
    bundle=ctx.validate_bundle(backend,runs,stage)
    if bundle:
        stdout=bundle['run']['output']['stdout']
    value=json.loads(stdout.strip().splitlines()[-1])
    assert value['workload']==mode
    if backend in ('staged','safe','vm') and mode=='write':
        assert not (work/'written').exists(),'staged writes reached lower workspace'
        assert len(list((stage/'upper/written').glob('*')))==256
    row=dict(suite='filesystem',workload=mode,backend=backend,trial=trial,
             wall_ms=wall,worker_ms=value['worker_ms'],correctness='passed',
             check=value['check'],python=value['python'],logs=str(root))
    if bundle:
        row['safety']=bundle.get('safety')
        row['observed_isolation']=bundle['run']['executor']['isolation']
    if measure:
        ctx.record(row)
    # Fixture and outcome evidence are retained; per-trial payload caches are disposable.
    shutil.rmtree(work)
    if stage.exists():
        shutil.rmtree(stage/'upper',ignore_errors=True)
