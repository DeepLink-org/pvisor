import struct
import json
import signal
import subprocess
import pytest
import filesystem_exec_probe
from filesystem_exec_probe import elf_interpreter, prepare_copies, run_owned_probe
from reference_baselines import digest


def test_loader_probe_rejects_static_or_invalid_elf_and_reads_declared_interpreter(tmp_path):
    path=tmp_path/'tool';path.write_bytes(b'not an executable')
    with pytest.raises(ValueError,match='ELF64'):elf_interpreter(path)
    data=bytearray(128);data[:6]=b'\x7fELF\x02\x01';struct.pack_into('<Q',data,32,64);struct.pack_into('<HH',data,54,56,0)
    path.write_bytes(data)
    with pytest.raises(ValueError,match='no dynamic interpreter'):elf_interpreter(path)
    interpreter=b'/lib64/ld-linux-x86-64.so.2\0';data.extend(interpreter)
    struct.pack_into('<HH',data,54,56,1);struct.pack_into('<I',data,64,3);struct.pack_into('<Q',data,72,128);struct.pack_into('<Q',data,96,len(interpreter))
    path.write_bytes(data);assert str(elf_interpreter(path))=='/lib64/ld-linux-x86-64.so.2'
    data[-1]=ord('x');path.write_bytes(data)
    with pytest.raises(ValueError,match='interpreter entry'):elf_interpreter(path)


def test_copy_preparation_uses_independent_inodes_and_rejects_changed_inputs(tmp_path):
    original=tmp_path/'tool';original.write_bytes(b'identical ELF input')
    work=tmp_path/'workspace';work.mkdir()
    inputs=[dict(path=str(original),copy_path='copies/0',sha256=digest(original))]
    prepare_copies(work,inputs)
    copied=work/'copies/0'
    assert copied.read_bytes()==original.read_bytes()
    assert (copied.stat().st_dev,copied.stat().st_ino)!=(original.stat().st_dev,original.stat().st_ino)
    original.write_bytes(b'changed source')
    with pytest.raises(ValueError,match='identical bytes'):prepare_copies(work,inputs)


def test_probe_timeout_kills_owned_group_and_retains_partial_evidence(tmp_path, monkeypatch):
    calls=[]
    class Process:
        pid=12345
        returncode=None
        def communicate(self, timeout=None):
            if timeout is not None:raise subprocess.TimeoutExpired(['probe'],timeout)
            self.returncode=-signal.SIGKILL
            return b'partial guest output',b'partial filesystem counters'
    def launch(command, **kwargs):
        assert kwargs['start_new_session'] is True
        return Process()
    monkeypatch.setattr(filesystem_exec_probe.subprocess,'Popen',launch)
    monkeypatch.setattr(filesystem_exec_probe.os,'killpg',lambda pid,sig:calls.append((pid,sig)))
    with pytest.raises(subprocess.TimeoutExpired):run_owned_probe(['probe'],tmp_path,{},tmp_path,timeout=1)
    assert calls==[(12345,signal.SIGKILL)]
    assert (tmp_path/'stdout.log').read_bytes()==b'partial guest output'
    assert (tmp_path/'stderr.log').read_bytes()==b'partial filesystem counters'
    assert json.loads((tmp_path/'command.json').read_text())['timed_out'] is True
