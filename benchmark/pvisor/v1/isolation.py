import json
from pathlib import Path
import shutil
import socket
import threading
import tempfile


def run(ctx):
    for backend in ('host','staged','safe','vm','container'):
        root=ctx.fresh('isolation-'+backend);work=root/'workspace';work.mkdir()
        outside=root/'outside';outside.mkdir();(outside/'secret').write_text('benchmark-secret')
        (work/'escape').symlink_to(outside,target_is_directory=True)
        shutil.copy2(Path(__file__).with_name('isolation_worker.py'),work/'worker.py')
        short=Path(tempfile.mkdtemp(prefix='pv-iso-'));(short/'outside').symlink_to(outside,target_is_directory=True)
        socket_path=str(short/'outside/socket')
        stop=threading.Event();listener=socket.socket(socket.AF_UNIX);listener.bind(socket_path);listener.listen(4);listener.settimeout(.1)

        def serve():
            while not stop.is_set():
                try:client,_=listener.accept()
                except TimeoutError:continue
                with client:client.sendall(b'benchmark-socket')

        thread=threading.Thread(target=serve,daemon=True);thread.start()
        try:
            stage=root/'stage';runs=root/'runs'
            argv=ctx.command(backend,work,stage,['/usr/bin/python3','worker.py',str(outside),str(work),socket_path])
            if backend=='vm':argv[argv.index('--rootfs')+1]=str(ctx.rootfs)
            home=root/'home';home.mkdir()
            wall,stdout,_=ctx.run(argv,cwd=work,env={'HOME':str(home),'PVISOR_RUN_HOME':str(runs),'XDG_CONFIG_HOME':str(root/'config')})
            bundle=ctx.validate_bundle(backend,runs,stage)
            value=json.loads(bundle['run']['output']['stdout'].strip().splitlines()[-1])
            value['host-outside-mutated']=(outside/'written').exists() or (outside/'symlink-written').exists()
            value['host-lower-mutated']=(work/'alias-written').exists()
            value['workspace-staged']=not (work/'staged-marker').exists()
            # Observe every path, then assess against the requested boundary.
            if backend in ('safe','vm','container'):
                assert not value['host-outside-mutated']
                assert not value['absolute-read'] and not value['symlink-read'] and not value['unix-socket']
            if backend in ('staged','safe','vm'):assert value['workspace-staged']
            ctx.record(dict(suite='isolation',workload='escape-fixtures',backend=backend,rootfs='prepared OCI' if backend in ('vm','container') else 'host',wall_ms=wall,observations=value,safety=bundle['safety'],observed_isolation=bundle['run']['executor']['isolation'],correctness='passed',logs=str(root)))
        finally:
            stop.set();thread.join();listener.close();shutil.rmtree(short)
