"""Small explicit API preflight; no dataset, VM, or generated command execution."""
import argparse
import json
import os
import time
from pathlib import Path

from agent_runner import model_key, model_kwargs, model_settings


def check(output):
    settings = model_settings()
    key = model_key(settings['profile'])
    os.environ['OPENAI_API_KEY'] = key
    os.environ.setdefault('LITELLM_LOCAL_MODEL_COST_MAP', 'True')
    import litellm

    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    (output / 'settings.json').write_text(json.dumps(settings, indent=2))
    messages = [dict(role='user', content='Call bash exactly once with command "printf pvisor-api-ok". '
                     'After receiving the tool result, reply exactly "pvisor-api-ok" without tools.')]
    tools = [dict(type='function', function=dict(name='bash', description='Run a shell command.',
             parameters=dict(type='object', properties={'command': {'type': 'string'}},
                             required=['command'], additionalProperties=False)))]
    responses = []
    started = time.monotonic()
    status = dict(status='failed', executed_commands=0)
    try:
        for step in range(2):
            response = litellm.completion(model='openai/' + settings['model'], messages=messages,
                                          tools=tools, **model_kwargs(settings))
            responses.append(response.model_dump())
            message = response.choices[0].message
            if step == 0:
                calls = message.tool_calls or []
                if len(calls) != 1 or calls[0].function.name != 'bash':
                    raise ValueError('expected one native bash tool call')
                if json.loads(calls[0].function.arguments).get('command') != 'printf pvisor-api-ok':
                    raise ValueError('unexpected tool arguments; no command was executed')
                messages.append(message.model_dump(exclude_none=True))
                messages.append(dict(role='tool', tool_call_id=calls[0].id, content='pvisor-api-ok'))
            elif message.tool_calls or (message.content or '').strip() != 'pvisor-api-ok':
                raise ValueError('unexpected final response after tool result')
        status['status'] = 'passed'
    except Exception as exc:
        status.update(error_type=type(exc).__name__, error=str(exc).replace(key, '[REDACTED]'))
        raise
    finally:
        status.update(duration_s=time.monotonic() - started, completed_requests=len(responses))
        (output / 'responses.json').write_text(json.dumps(responses, indent=2).replace(key, '[REDACTED]'))
        (output / 'result.json').write_text(json.dumps(status, indent=2))
    return status


if __name__ == '__main__':
    os.umask(0o077)
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if not args.output.resolve().is_relative_to('/data'):
        parser.error('preflight evidence must stay under /data')
    try:
        print(json.dumps(check(args.output)))
    except Exception as exc:
        print(json.dumps(dict(status='failed', error_type=type(exc).__name__,
                              detail='See result.json in the output directory; check host configuration if absent.')))
        raise SystemExit(1) from None
