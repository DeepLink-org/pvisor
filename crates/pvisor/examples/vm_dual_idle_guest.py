"""Dual-vCPU handoff probe payload; no model-client idle declarations."""
import hashlib
import json
import multiprocessing as mp
import os
import socket
import sys
import threading
import time
import urllib.request

mode, rounds, address, port = sys.argv[1], int(sys.argv[2]), sys.argv[3], int(sys.argv[4])
assert os.sched_getaffinity(0) == {0, 1}
cmdline = open('/proc/cmdline').read()
assert not any(flag in cmdline for flag in ('psi=0', 'psi_idle_quiesce=1',
                                          'psi_idle_defer=1', 'pvisor_watchdog_defer=1'))
assert os.path.exists('/proc/pressure/cpu')
print(json.dumps(dict(cpus=2, mode=mode, cmdline=cmdline)), flush=True)
ctx = mp.get_context('fork')
data = bytes(range(256)) * 4096
expected = hashlib.sha256(data).hexdigest()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def interrupts():
    counts = {}
    for line in open('/proc/interrupts'):
        name, sep, value = line.partition(':')
        if sep and name.strip() in ('RES', 'CAL'):
            counts[name.strip()] = sum(int(n) for n in value.split()[:2])
    return counts


def busy(cpu, stop, ready):
    os.sched_setaffinity(0, {cpu})
    # Parent starts the request only after this worker is running. It remains
    # runnable until AFTER the model response, avoiding a fixed-duration race.
    hashlib.sha256(data).digest()
    ready.send(True)
    while not stop.value:
        hashlib.sha256(data).digest()


def ping(cpu, link):
    os.sched_setaffinity(0, {cpu})
    link.send(True)
    while link.recv() != 'stop':
        link.send(time.monotonic_ns())


def timer(cpu, delay, link):
    os.sched_setaffinity(0, {cpu})
    link.send(True)
    link.recv()
    start = time.monotonic()
    time.sleep(delay)
    link.send((time.monotonic() - start) * 1000)


for cycle in range(rounds):
    cpu = cycle % 2
    os.sched_setaffinity(0, {cpu})
    workers, threads, links = [], [], []
    times = {}
    before = interrupts()
    stop = ctx.RawValue('b', False)
    if mode == 'busy':
        parent, child = ctx.Pipe()
        worker = ctx.Process(target=busy, args=(1-cpu, stop, child))
        worker.start(); workers.append(worker)
        assert parent.poll(5) and parent.recv()
    elif mode == 'ipi':
        parent, child = ctx.Pipe()
        worker = ctx.Process(target=ping, args=(1-cpu, child))
        worker.start(); workers.append(worker)
        assert parent.poll(5) and parent.recv()
        def exchange():
            os.sched_setaffinity(0, {cpu})
            latencies = []
            for _ in range(8):
                time.sleep(.04)
                start = time.monotonic_ns()
                parent.send('ping')
                assert parent.poll(2), 'cross-CPU wake lost'
                end = parent.recv()
                latencies.append((end-start)/1e6)
            parent.send('stop')
            times['ipi_ms'] = latencies
        thread = threading.Thread(target=exchange); thread.start(); threads.append(thread)
    elif mode == 'timer':
        for target, delay in [(cpu, .08), (1-cpu, .25)]:
            parent, child = ctx.Pipe()
            worker = ctx.Process(target=timer, args=(target, delay, child))
            worker.start(); workers.append(worker)
            assert parent.poll(5) and parent.recv()
            links.append((parent, delay))
        for link, _ in links:
            link.send(True)
    elif mode in ('network', 'race', 'ingress'):
        sock = socket.create_connection((address, port), timeout=5)
        assert sock.recv(1) == b'R'
        def receive():
            os.sched_setaffinity(0, {1-cpu})
            assert sock.recv(1) == b'W'
            times['event_ms'] = (time.monotonic()-started)*1000
            sock.close()
        thread = threading.Thread(target=receive); thread.start(); threads.append(thread)
    started = time.monotonic()
    request = urllib.request.Request(
        os.environ['OPENAI_BASE_URL'].rstrip('/')+'/chat/completions',
        data=json.dumps(dict(model='fixture', messages=[dict(role='user', content=str(cycle))],
                             stream=False)).encode(),
        headers={'Content-Type': 'application/json', 'Authorization': 'Bearer '+os.environ['OPENAI_API_KEY']})
    try:
        with opener.open(request, timeout=10) as response:
            result = json.load(response)
        times['model_ms'] = (time.monotonic()-started)*1000
        assert result['choices'][0]['message']['content'] == 'idle-ok'
    finally:
        stop.value = True
    for thread in threads:
        thread.join(5)
        assert not thread.is_alive()
    for link, delay in links:
        assert link.poll(2), 'timer failed to wake CPU'
        elapsed = link.recv()
        assert delay*1000 <= elapsed < delay*1000+100, elapsed
        times.setdefault('timer_ms', []).append(elapsed)
    for worker in workers:
        worker.join(5)
        assert not worker.is_alive() and worker.exitcode == 0
    after = interrupts()
    times['interrupt_delta'] = {k: after[k]-before.get(k, 0) for k in after}
    if mode == 'ipi':
        assert len(times.get('ipi_ms', [])) == 8, times
        assert max(times['ipi_ms']) < 100, times
        assert sum(times['interrupt_delta'].values()) > 0, times
    if mode in ('network', 'race', 'ingress'):
        assert 'event_ms' in times, times
    if mode in ('network', 'ingress'):
        assert times['event_ms'] < times['model_ms']-30, times
    assert hashlib.sha256(data).hexdigest() == expected
    print(json.dumps(dict(cycle=cycle, mode=mode, **times)), flush=True)
    time.sleep(.03)
print('idle-guest-ok', flush=True)
