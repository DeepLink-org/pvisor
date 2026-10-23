"""SweEval Agent adapter reusing the upstream mini Agent in a host subprocess."""
import asyncio
import json
import os
import signal
import sys
from pathlib import Path

from sweeval.agents.base import BaseAgent
from sweeval.agents.mini_swe_agent import MiniSweAgent


class HostMiniAgent(BaseAgent):
    SUPPORTS_ATIF = True
    SUPPORTS_ACCOUNTING = True

    def __init__(self, *args, max_steps=200, model_kwargs=None, **kwargs):
        super().__init__(*args, **kwargs)
        self.max_steps = max_steps
        self.model_kwargs = model_kwargs or {}

    @staticmethod
    def name():
        return 'pvisor-mini-swe-agent'

    def version(self):
        return '2.4.6'

    async def setup(self, environment):
        await environment.run_healthcheck()

    async def run(self, instruction, environment, context):
        root = self.logs_dir
        root.mkdir(parents=True, exist_ok=True)
        uname = await environment.exec('uname -r')
        config = dict(control=str(environment.client.root), token=environment.token,
                      events=str(environment.events), instruction=instruction,
                      model_kwargs=self.model_kwargs, model=self.model_name, base_url=self.model_base_url,
                      cwd=environment.workdir, guest_release=uname.stdout.strip(),
                      tool_env=environment._merged_env(None), tool_user=environment.default_user,
                      max_steps=self.max_steps,
                      trajectory=str(root / 'mini-swe-agent.trajectory.json'),
                      outcome=str(root / 'outcome.json'))
        path = root / 'host-agent.json'
        path.write_text(json.dumps(config))
        path.chmod(0o600)
        process = None
        try:
            with (root / 'mini-swe-agent.txt').open('wb') as stream:
                process = await asyncio.create_subprocess_exec(
                    sys.executable, str(Path(__file__).with_name('agent_runner.py')), str(path),
                    stdout=stream, stderr=stream, start_new_session=True)
                code = await process.wait()
            if code:
                raise RuntimeError(f'host mini Agent exited {code}; see mini-swe-agent.txt')
        finally:
            if process is not None and process.returncode is None:
                os.killpg(process.pid, signal.SIGKILL)
                await process.wait()
                environment.client.cancel_pending()
                for _ in range(150):
                    pending = [p for p in environment.client.root.glob('*.request.json')
                               if not environment.client.exists(
                                   p.name.removesuffix('.request.json') + '.result.json')]
                    if not pending:
                        break
                    await asyncio.sleep(.1)
                else:
                    await environment.stop()
            keys = {os.environ.get('OPENAI_API_KEY', '')} - {''}
            if keys:
                for output in root.glob('*'):
                    if output.is_file() and output.suffix in ('.json', '.txt'):
                        text = output.read_text(errors='replace')
                        for key in keys:
                            text = text.replace(key, '[REDACTED]')
                        output.write_text(text)

    def populate_context_post_run(self, context):
        MiniSweAgent.populate_context_post_run(self, context)

    def convert_trajectory_post_run(self):
        return MiniSweAgent.convert_trajectory_post_run(self)
