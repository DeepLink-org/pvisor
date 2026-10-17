"""Synthetic SweEval trial on real KVM, using a local HTTP model fixture.

No external model or real SWE score. Exercises the unchanged SweEval trial,
mini Agent, Chat Completions wire protocol, VM tools and shared verifier.
"""
import argparse
import asyncio
import json
import os
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from config import load_config
from run import evaluate


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--profile', choices=['qwen', 'glm'], default='qwen')
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--rootfs', required=True)
    parser.add_argument('--python', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, mode=0o700, exist_ok=False)
    config = load_config(args.config)
    task = args.output / 'tasks/fixture'
    (task / 'tests').mkdir(parents=True)
    (task / 'task.toml').write_text('''schema_version = "1.0"
[task]
name = "local/resident-adapter-smoke"
[environment]
docker_image = "local-fixture"
cpus = 2
memory_mb = 1024
workdir = "/tmp"
''')
    (task / 'instruction.md').write_text('Write the requested marker and submit.')
    (task / 'tests/test.sh').write_text('''#!/bin/bash
set -eu
test "$(cat /tmp/resident-agent-marker)" = adapter-ok
echo '{"reward":1}' > /logs/verifier/reward.json
''')
    (task / 'solution').mkdir()
    (task / 'solution/solve.sh').write_text(
        '#!/bin/bash\nset -eu\nprintf adapter-ok > /tmp/resident-agent-marker\n')
    prepared = args.output / 'prepared.json'
    prepared.write_text(json.dumps(dict(source_image='local-fixture', image_digest='local-fixture', rootfs=args.rootfs,
                                        python=args.python, workdir='/tmp', environment={})))
    config.update(task_dir=str(task.parent), max_steps=3,
                  tasks=[dict(name='fixture', directory='fixture', prepared=str(prepared))])
    oracle_output = args.output / 'oracle'
    asyncio.run(evaluate(config, oracle_output, oracle=True))
    config['require_oracle'] = str(oracle_output / 'oracle-gate.json')
    received = []
    model_id = 'agents-a1' if args.profile == 'qwen' else 'glm-5.3-flash'
    prefix = '/v1' if args.profile == 'qwen' else '/api/paas/v4'

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            received.append(dict(path=self.path, body=body))
            command = ('printf adapter-ok > /tmp/resident-agent-marker' if len(received) == 1
                       else 'echo COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT')
            response = dict(id='fixture-' + str(len(received)), object='chat.completion',
                            created=int(time.time()), model=model_id,
                            choices=[dict(index=0, finish_reason='tool_calls', message=dict(
                                role='assistant', content='fixture', reasoning_content='fixture reasoning',
                                tool_calls=[dict(id='call-' + str(len(received)), type='function',
                                                 function=dict(name='bash', arguments=json.dumps(
                                                     dict(command=command))))]))],
                            usage=dict(prompt_tokens=10, completion_tokens=20, total_tokens=30))
            raw = json.dumps(response).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    previous = {key: os.environ.get(key) for key in (
        'SWE_MODEL_PROFILE', 'OPENAI_BASE_URL', 'OPENAI_API_KEY', 'OPENAI_MODEL')}
    os.environ.update(SWE_MODEL_PROFILE=args.profile,
                      OPENAI_BASE_URL=f'http://127.0.0.1:{server.server_port}{prefix}',
                      OPENAI_API_KEY='local-fixture-only', OPENAI_MODEL=model_id)
    try:
        results = asyncio.run(evaluate(config, args.output / 'run'))
    finally:
        server.shutdown()
        server.server_close()
        for key, value in previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        (args.output / 'wire.json').write_text(json.dumps(received, indent=2))
    assert len(received) == 2, results
    assert all(r['path'] == prefix + '/chat/completions' for r in received)
    assert all(r['body']['model'] == model_id for r in received)
    if args.profile == 'glm':
        assert received[0]['body']['thinking']['clear_thinking'] is False
    else:
        assert all('thinking' not in r['body'] and 'reasoning_effort' not in r['body']
                   for r in received)
    assert any(m.get('reasoning_content') == 'fixture reasoning'
               for m in received[1]['body']['messages']), 'reasoning missing from second request'
    assert results[0]['verifier_result']['rewards']['reward'] == 1, results
    assert results[0]['exception_info'] is None, results
    (args.output / 'passed.json').write_text(json.dumps(dict(
        status='passed', profile=args.profile, external_model_calls=0, fixture_model_calls=len(received),
        real_kvm=True, verifier_reward=1)))


if __name__ == '__main__':
    os.umask(0o077)
    main()
