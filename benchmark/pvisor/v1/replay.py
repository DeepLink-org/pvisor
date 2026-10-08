"""Twenty synthetic native-format prefixes per adapter, three fresh repetitions.

Benchmark: B-REPLAY (benchmark/README.md#b-replay), role user-facing.
Motivation: training and reproduction need trajectories restored to the same
starting point without side effects.
Conclusion sought: every supported format passes fidelity checks, prepare
runs no tool and leaves the workspace untouched, and its cost per trajectory.
Design: fixed native-format samples per adapter; prefix structure, tool
arguments, observations and workspace state verified.
"""

import json
import shutil
import random
import traceback

from .common import digest


def trajectory(agent, task, command):
    next_command = f"cat marker-{task}.txt"
    if agent == 'swe-agent':
        steps=[];history=[dict(role='user',content=f'fixture {task}')]
        for i,cmd in enumerate((command,next_command)):
            steps.append(dict(action=cmd,observation='historical observation',thought=f'fixture-text-{task}-{i}',state={}))
            history.extend([dict(role='assistant',content=f'fixture-text-{task}-{i}',tool_calls=[dict(id=f'call-{i}')]),
                            dict(role='tool',content='historical observation',tool_call_id=f'call-{i}')])
        return dict(trajectory=steps,history=history,replay_config={}),False
    if agent == "claude-code":
        events = [
            {
                "type": "user",
                "uuid": "user-1",
                "parentUuid": None,
                "isSidechain": False,
                "sessionId": "session-1",
                "version": "2.1.220",
                "message": {"role": "user", "content": f"fixture {task}"},
            }
        ]
        parent = "user-1"
        for i, cmd in enumerate((command, next_command)):
            assistant = f"assistant-{i}"
            result = f"result-{i}"
            events.append(
                {
                    "type": "assistant",
                    "uuid": assistant,
                    "parentUuid": parent,
                    "isSidechain": False,
                    "sessionId": "session-1",
                    "version": "2.1.220",
                    "message": {
                        "id": f"message-{i}",
                        "role": "assistant",
                        "content": [
                            {"type": "text", "text": f"fixture-text-{task}-{i}"},
                            {
                                "type": "tool_use",
                                "id": f"tool-{i}",
                                "name": "Bash",
                                "input": {"command": cmd},
                            },
                        ],
                    },
                }
            )
            events.append(
                {
                    "type": "user",
                    "uuid": result,
                    "parentUuid": assistant,
                    "sourceToolAssistantUUID": assistant,
                    "isSidechain": False,
                    "sessionId": "session-1",
                    "version": "2.1.220",
                    "message": {
                        "role": "user",
                        "content": [
                            {
                                "type": "tool_result",
                                "tool_use_id": f"tool-{i}",
                                "content": "historical observation",
                            }
                        ],
                    },
                }
            )
            parent = result
        return events, True
    if agent == "codex":
        events = [
            {"type": "session_meta", "payload": {"id": "sess-benchmark"}},
            {
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": f"fixture {task}"}],
                },
            },
        ]
        for i, cmd in enumerate((command, next_command)):
            events.append(
                {
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": f"fixture-text-{task}-{i}"}],
                    },
                }
            )
            events += [
                {
                    "type": "response_item",
                    "payload": {
                        "type": "function_call",
                        "call_id": f"call-{i}",
                        "name": "exec_command",
                        "arguments": json.dumps({"cmd": cmd}),
                    },
                },
                {
                    "type": "response_item",
                    "payload": {
                        "type": "function_call_output",
                        "call_id": f"call-{i}",
                        "output": "historical observation",
                    },
                },
            ]
        return events, True
    if agent == "opencode":
        events = [
            {
                "type": "user",
                "sessionID": "ses-benchmark",
                "parts": [{"type": "text", "text": f"fixture {task}"}],
            }
        ]
        for i, cmd in enumerate((command, next_command)):
            events += [
                {"type": "step_start", "sessionID": "ses-benchmark"},
                {
                    "type": "tool_use",
                    "sessionID": "ses-benchmark",
                    "part": {
                        "type": "tool",
                        "tool": "bash",
                        "callID": f"call-{i}",
                        "state": {
                            "status": "completed",
                            "input": {"command": cmd},
                            "output": "historical observation",
                        },
                    },
                },
                {
                    "type": "step_finish",
                    "sessionID": "ses-benchmark",
                    "part": {"reason": "tool-calls"},
                },
            ]
        return events, True
    if agent == "mini-swe-agent":
        messages = [{"role": "user", "content": f"fixture {task}", "extra": {}}]
        for i, cmd in enumerate((command, next_command)):
            messages += [
                {
                    "role": "assistant",
                    "content": f"fixture-text-{task}-{i}",
                    "extra": {
                        "response": {},
                        "actions": [{"tool_call_id": f"call-{i}", "command": cmd}],
                    },
                },
                {"role": "tool", "content": "historical observation", "extra": {"returncode": 0}},
            ]
        return {
            "trajectory_format": "mini-swe-agent-1.1",
            "info": {
                "mini_version": "2.4.6",
                "config": {"model": {}, "agent": {}, "environment": {}},
            },
            "messages": messages,
        }, False
    if agent == "openhands":
        events = [
            {"id": 0, "source": "user", "action": "message", "args": {"content": f"fixture {task}"}}
        ]
        for i, cmd in enumerate((command, next_command)):
            events += [
                {
                    "id": 1 + i * 2,
                    "source": "agent",
                    "action": "run",
                    "args": {"command": cmd, "thought": f"fixture-text-{task}-{i}"},
                },
                {
                    "id": 2 + i * 2,
                    "source": "environment",
                    "observation": "run",
                    "cause": 1 + i * 2,
                    "message": "historical observation",
                    "args": {"command": cmd, "metadata": {"exit_code": 0}},
                },
            ]
        return events, False
    events = [
        {
            "type": "message_end",
            "message": {"role": "user", "content": f"fixture {task}", "timestamp": 1},
        }
    ]
    for i, cmd in enumerate((command, next_command)):
        events.append(
            {
                "type": "turn_end",
                "message": {
                    "role": "assistant",
                    "provider": "benchmark",
                    "model": "fixture",
                    "api": "openai-completions",
                    "content": [
                        {"type": "text", "text": f"fixture-text-{task}-{i}"},
                        {
                            "type": "toolCall",
                            "id": f"call-{i}",
                            "name": "bash",
                            "arguments": {"command": cmd},
                        },
                    ],
                    "stopReason": "toolUse",
                    "usage": {"input": 1, "output": 1},
                    "timestamp": 2 + i * 2,
                },
                "toolResults": [
                    {
                        "role": "toolResult",
                        "toolCallId": f"call-{i}",
                        "toolName": "bash",
                        "content": [{"type": "text", "text": "historical observation"}],
                        "isError": False,
                        "timestamp": 3 + i * 2,
                    }
                ],
            }
        )
    return events, True


def nested_strings(value):
    if isinstance(value,dict):
        return [s for child in value.values() for s in nested_strings(child)]
    if isinstance(value,list):
        return [s for child in value for s in nested_strings(child)]
    if isinstance(value,str):
        try:
            parsed=json.loads(value)
            if isinstance(parsed,(dict,list)):return [value,*nested_strings(parsed)]
        except (ValueError,TypeError):pass
        return [value]
    return []


def validate_prefix(manifest,output,agent,command,next_command,source_sha):
    boundary=manifest['boundary']
    if manifest['agent']['name']!=agent or manifest['source']['sha256']!=source_sha or boundary['after_step']!=1 or boundary['tool_calls']!=1 or boundary['complete_tool_batch'] is not True:
        raise ValueError('wrong source, adapter or complete prefix boundary')
    batches=manifest['batches']
    if len(batches)!=1 or len(batches[0]['tool_calls'])!=1:raise ValueError('wrong prefix structure')
    call=batches[0]['tool_calls'][0];arguments=call['arguments']
    if arguments.get('command',arguments.get('cmd',arguments.get('raw_action')))!=command:raise ValueError('prefix tool arguments differ')
    filename={'mini-swe-agent':'prepared-prefix.json','openhands':'prepared-replay-events.json','swe-agent':'prepared-prefix.traj'}.get(agent,'prepared-prefix.jsonl')
    raw=(output/'native'/filename).read_text()
    native=[json.loads(line) for line in raw.splitlines() if line.strip()] if filename.endswith('.jsonl') else json.loads(raw)
    strings=nested_strings(native)
    if command not in strings or 'historical observation' not in strings or next_command in strings:
        raise ValueError('prepared native prefix lost tool/observation or included action beyond boundary')


def replay_trial(ctx,binary,agent,task,trial):
    command=f'printf fixture-{task} > marker-{task}.txt'
    source,jsonl=trajectory(agent,task,command)
    root=ctx.fresh(f'replay-{agent}-{task}');work=root/'workspace';work.mkdir()
    (work/'existing').write_text('existing workspace must survive preparation\n')
    path=root/('trajectory.jsonl' if jsonl else 'trajectory.json')
    path.write_text(('\n'.join(json.dumps(item) for item in source) if jsonl else json.dumps(source))+'\n')
    argv=[str(binary),'--agent',agent,'--trajectory',str(path),'--after-step','1','--prepare-only',
          '--workspace',str(work),'--state-dir',str(root/'state'),'--output-dir',str(root/'output')]
    wall,_,_=ctx.run(argv,cwd=work)
    results=list((root/'output').glob('*/result.json'))
    if len(results)!=1:raise ValueError('missing unique replay result')
    result=json.loads(results[0].read_text());manifest=json.loads(results[0].with_name('manifest.json').read_text())
    if result['failure'] is not None or result['replayed_tool_calls']!=0:raise ValueError('prepare-only executed tools or failed')
    validate_prefix(manifest,results[0].parent,agent,command,f'cat marker-{task}.txt',digest(path))
    if list(work.iterdir())!=[work/'existing'] or (work/'existing').read_text()!='existing workspace must survive preparation\n':
        raise ValueError('prepare-only modified workspace or executed an operation')
    ctx.record(dict(suite='replay',workload='prepare-only',agent=agent,profile=manifest['agent']['profile'],task=task,trial=trial,
        wall_ms=wall,prefix_arguments_exact=True,native_observation_preserved=True,source_digest_exact=True,
        executed_tools=0,correctness='passed',logs=str(root)))


def run(ctx):
    binary=ctx.output/'bin/pvisor-replay';shutil.copy2(ctx.args.replay_binary.resolve(),binary)
    receipt=ctx.metadata.get('binary_build')
    if not receipt or digest(binary)!=receipt['binaries']['pvisor-replay']['sha256']:
        raise ValueError('replay binary must match frozen source build receipt')
    agents=('claude-code','codex','opencode','mini-swe-agent','openhands','pi-agent','swe-agent')
    ctx.metadata['replay_binary_sha256']=digest(binary)
    ctx.metadata['replay_protocol']=dict(model='none; no model requests',tasks=20,repetitions=min(ctx.args.samples,3),mode='prepare-only',
        adapters=list(agents),fixtures='synthetic native formats at adapter-declared versions; not installed CLI executions or model task success',
        integrity='exact source digest, complete one-batch boundary, tool arguments, native historical observation and unchanged workspace',
        order='seeded shuffled adapter/task conditions each repetition; failed fidelity excluded from timing and retained')
    ctx.save();rng=random.Random(ctx.args.seed)
    for trial in range(min(ctx.args.samples,3)):
        cases=[(agent,task) for agent in agents for task in range(20)];rng.shuffle(cases)
        for agent,task in cases:
            try:replay_trial(ctx,binary,agent,task,trial)
            except Exception as error:
                failure=ctx.capabilities.setdefault('replay/'+agent,dict(state='failed',failures=[]))
                failure['failures'].append(dict(task=task,trial=trial,error=str(error),traceback=traceback.format_exc()));ctx.save()
        print(f'replay: repetition {trial+1} of {min(ctx.args.samples,3)} complete',flush=True)
