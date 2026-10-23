"""Load portable benchmark paths; never expand model credentials into JSON."""
import json
import os
import re
from pathlib import Path
from string import Template


def load_config(path):
    config = json.loads(Path(path).read_text())
    def expand(value):
        try:
            return Template(value).substitute(os.environ)
        except (KeyError, ValueError) as exc:
            raise ValueError('unresolved variable in benchmark path') from exc
    for key in ('task_dir', 'binary', 'firmware', 'inputs', 'umoci', 'require_oracle'):
        if key in config:
            config[key] = expand(config[key])
    names = set()
    for entry in config['tasks']:
        name = entry['name']
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]*', name) or name in names:
            raise ValueError('task names must be unique path components')
        names.add(name)
        entry['prepared'] = expand(entry['prepared'])
    if not names:
        raise ValueError('select at least one task')
    return config

def validate_oracle(config, plan):
    """Require a successful reference run with the same images and VM override."""
    if not config.get('require_oracle'):
        raise ValueError('require_oracle must point to a successful reference run')
    gate = json.loads(Path(config['require_oracle']).read_text())
    if (gate.get('status') != 'passed' or gate.get('images') != {
            e['name']: p['image_digest'] for e, _, p in plan} or
            gate.get('krun_enomem_workaround') != config.get('krun_enomem_workaround')):
        raise ValueError('matching task oracle validation is required')
