#!/usr/bin/env python3
"""Prepare an isolated, credential-free ZCode integration fixture."""

import hashlib
import json
import os
import sys
from pathlib import Path


def write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + "\n")


work = Path(sys.argv[1]).resolve()
node = Path(os.environ["ZCODE_NODE"]).resolve(strict=True)
entry = Path(os.environ["ZCODE_ENTRY"]).resolve(strict=True)
builtin = Path(os.environ["ZCODE_BUILTIN_PROVIDER_CONFIG_FILE"]).resolve(strict=True)
binary = Path(sys.argv[3]).resolve(strict=True)
write_json(
    work / "runtime-manifest.json",
    {
        label: {
            "path": str(path),
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        }
        for label, path in {
            "node": node,
            "zcode": entry,
            "builtin_provider": builtin,
            "pvisor": binary,
        }.items()
    },
)
base = work / "base"
base.mkdir(parents=True, exist_ok=True)
(base / "original.txt").write_text("original\n")
write_json(
    base / "zcode.json",
    {
        "plugins": {"enabled": False},
        "features": {"memory": False, "mcp": False, "skill": False},
        "memory": {"use": False},
        "skills": {"enabled": False, "includeInstructions": False},
    },
)
model = "pvisor-mock"
provider = {
    "schemaVersion": 1,
    "config": {
        "providerOrder": ["pvisor"],
        "defaultModelSelection": {"providerId": "pvisor", "modelId": model},
        "providerConfigRules": {
            "providerRules": [
                {
                    "providerId": "pvisor",
                    "providerName": "pVisor mock",
                    "enabled": True,
                    "config": {
                        "group": "standard-personal",
                        "access": {"type": "api-key", "apiKey": "mock-no-secret"},
                        "api": {
                            "type": "openai-chat-completions",
                            "baseUrl": sys.argv[2],
                        },
                        "personalModelIds": [model],
                    },
                }
            ]
        },
        "modelConfigRules": {
            "providerModelRules": [
                {
                    "providerId": "pvisor",
                    "modelId": model,
                    "config": {
                        "enabled": True,
                        "properties": {
                            "requiresMfjsToolSchema": False,
                            "contextWindow": 128000,
                            "inputFormat": {
                                "supportsText": True,
                                "supportsImage": False,
                                "supportsVideo": False,
                                "supportsAudio": False,
                                "supportsPdf": False,
                            },
                            "outputFormat": {"supportsText": True},
                            "supportsToolCall": True,
                            "supportsJsonSchemaOutput": False,
                            "supportsNativeWebSearch": False,
                            "supportsMidConversationSystem": False,
                        },
                        "optionSpecs": {
                            "reasoningLevel": {"values": ["none"], "map": "{}"},
                            "maxOutputTokens": {"max": 4096, "map": "{}"},
                        },
                    },
                }
            ],
            "manualProviderModelRules": [],
        },
    },
}
state = work / "state"
write_json(state / "provider.json", provider)
# The test calls the installed command unchanged. Only the trusted pVisor
# profile and ordinary ZCode config/environment select the test provider.
runtime = Path(os.environ["ZCODE_RUNTIME_ROOT"]).resolve(strict=True)
config = work / "config/pvisor/agents/zcode.toml"
config.parent.mkdir(parents=True)
config.write_text("\n".join(
    f"[[run.filesystem]]\npath = {json.dumps(str(path))}\naccess = '{access}'\n"
    for path, access in [(runtime, "read"), (node.parent.parent, "read"), (state, "read_write")]
))
write_json(
    work / "base-before.json",
    {
        str(p.relative_to(base)): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in base.rglob("*")
        if p.is_file()
    },
)
