"""Run upstream mini-swe-agent on the host with a VM tool environment.

The Agent loop, prompts, tools, native serialization and model parsing remain
upstream 2.4.6. Only environment execution and per-request timing are adapted.
"""
import json
import os
import sys
from pathlib import Path
from urllib.parse import urlsplit

from transport import Client, event


def api_base(value):
    parsed = urlsplit(value)
    if parsed.scheme not in ('http', 'https') or not parsed.hostname:
        raise ValueError('API base must be an HTTP(S) API base URL')
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError('API base must not contain credentials, query or fragment')
    base = value.rstrip('/')
    if base.endswith(('/chat/completions', '/responses')):
        raise ValueError('supply API base, not the chat/completions or responses endpoint')
    return base


def model_settings(environ=None):
    """Non-secret configuration; Qwen is the default, GLM is opt-in."""
    env = os.environ if environ is None else environ
    profile = env.get('SWE_MODEL_PROFILE', 'qwen')
    if profile not in ('qwen', 'glm'):
        raise ValueError('SWE_MODEL_PROFILE must be qwen or glm')
    if profile == 'qwen':
        base = env.get('OPENAI_BASE_URL', 'https://token.pjlab.org.cn/v1')
        model = env.get('OPENAI_MODEL', 'agents-a1')
    else:
        base = env.get('OPENAI_BASE_URL') or env.get('GLM_BASE_URL', '')
        model = env.get('OPENAI_MODEL') or env.get('GLM_MODEL', 'glm-5.3-flash')
    if not model or model.startswith('openai/'):
        raise ValueError('supply the service model ID without the internal openai/ prefix')
    return dict(profile=profile, base_url=api_base(base), model=model)


def model_key(profile):
    key = os.environ.get('OPENAI_API_KEY', '')
    if not key and profile == 'glm':
        key = os.environ.get('GLM_API_KEY', '')
    if not key:
        raise ValueError('OPENAI_API_KEY must be set in the host environment')
    return key


def model_kwargs(config):
    profile = config.get('profile', 'qwen')
    if profile not in ('qwen', 'glm'):
        raise ValueError('unknown model profile')
    kwargs = dict(api_base=api_base(config['base_url']),
                  timeout=config.get('request_timeout', 120), num_retries=0,
                  max_tokens=config.get('max_tokens', 16384))
    if profile == 'glm':
        kwargs.update(temperature=1, top_p=.95,
                      extra_body={'thinking': {'type': 'enabled', 'clear_thinking': False},
                                  'reasoning_effort': config.get('reasoning_effort', 'max')})
    # Qwen uses the lab service's defaults: do not guess thinking/template flags.
    return kwargs


def run(config):
    os.environ.setdefault('LITELLM_LOCAL_MODEL_COST_MAP', 'True')
    # Avoid reading a host-wide .env or creating state outside the run directory.
    os.environ['MSWEA_GLOBAL_CONFIG_DIR'] = str(Path(config['outcome']).parent / 'mini-config')
    import importlib.resources

    import yaml
    from minisweagent.agents.default import DefaultAgent
    from minisweagent.environments.local import LocalEnvironment
    from minisweagent.models import get_model

    client = Client(config['control'], config['token'], config['events'])

    class VMEnvironment(LocalEnvironment):
        def execute(self, action, cwd='', *, timeout=None):
            result = client.call(action['command'], cwd=cwd or self.config.cwd,
                                 env=self.config.env, timeout=timeout or self.config.timeout,
                                 user=config.get('tool_user'))
            output = dict(output=result['stdout'] + result['stderr'],
                          returncode=result['return_code'],
                          exception_info='command timed out' if result['timed_out'] else '')
            self._check_finished(output)
            return output

        def get_template_vars(self, **kwargs):
            # Describe the execution environment, never expose host env/secrets
            # through LocalEnvironment's default os.environ template variables.
            return dict(cwd=self.config.cwd, system='Linux', release=config['guest_release'],
                        version='pVisor guest', machine='x86_64', **kwargs)

        def serialize(self):
            value = super().serialize()
            # Replays execute this native local environment *inside* their
            # fresh guest. Do not serialize controller IPC paths or class names.
            value['info']['config']['environment_type'] = 'minisweagent.environments.local.LocalEnvironment'
            return value

    defaults = yaml.safe_load(importlib.resources.files('minisweagent.config').joinpath('mini.yaml').read_text())
    model_config = dict(defaults['model'])
    model_config.update(model_class='litellm', model_name='openai/' + config['model'],
                        cost_tracking='ignore_errors')
    model_config['model_kwargs'] = model_kwargs(config)
    model = get_model(config=model_config)
    query = model._query

    def measured_query(messages, **kwargs):
        event(config['events'], 'model_request_start')
        try:
            response = query(messages, **kwargs)
        except BaseException as exc:
            event(config['events'], 'model_request_error', error_type=type(exc).__name__)
            raise
        event(config['events'], 'model_request_end', usage=response.usage.model_dump()
              if response.usage else None)
        return response

    model._query = measured_query
    env = VMEnvironment(cwd=config['cwd'], timeout=config.get('tool_timeout', 120),
                        env={**defaults['environment']['env'], **config.get('tool_env', {})})
    agent_config = dict(defaults['agent'])
    agent_config.pop('mode', None)
    agent_config.update(step_limit=config.get('max_steps', 200), cost_limit=0,
                        wall_time_limit_seconds=config.get('wall_time', 10800),
                        output_path=Path(config['trajectory']))
    agent = DefaultAgent(model, env, **agent_config)
    outcome = agent.run(config['instruction'])
    Path(config['outcome']).write_text(json.dumps(outcome))


if __name__ == '__main__':
    # The key is inherited only by this host process, never in the config file,
    # native trajectory model kwargs or guest environment.
    config = json.loads(Path(sys.argv[1]).read_text())
    os.environ['OPENAI_API_KEY'] = model_key(config.get('profile', 'qwen'))
    os.environ.setdefault('MSWEA_MODEL_RETRY_STOP_AFTER_ATTEMPT', '2')
    run(config)
