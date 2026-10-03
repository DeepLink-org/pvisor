#!/usr/bin/env python3
"""Check documented config field names/types against the serialized Rust structs.

This is a field-coverage guard, not a Rust parser or a JSON Schema generator.
Defaults and CLI precedence still require behavioral review. Fail on unfamiliar
field syntax rather than silently omit it.
"""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONFIG = "crates/pvisor/src/config.rs"
CORE = "crates/pvisor-core/src/"
GROUPS = [
    (CONFIG, "RunConfig", ""),
    (CONFIG, "RunSettings", "run."),
    (CONFIG, "ContainerSettings", "container."),
    (CONFIG, "ContainerMount", "container.mounts[]."),
    (CONFIG, "VmSettings", "vm."),
    (CONFIG, "OverlayFsSettings", "overlayfs."),
    (CONFIG, "FilesystemMount", "overlayfs.mount[]."),
    (CONFIG, "FilesystemAccessRule", "overlayfs.access[]."),
    (CONFIG, "OverlayNetSettings", "overlaynet."),
    (CONFIG, "GatewaySettings", "gateway."),
    (CONFIG, "RecordSettings", "record."),
    (CORE + "execution.rs", "ResourceLimits", "run.resource_limits."),
    (CORE + "execution.rs", "FilesystemCapability", "run.filesystem[]."),
    (CORE + "execution.rs", "NetworkAccessRule", "network_rule."),
    (CORE + "execution.rs", "NetworkBandwidthLimit", "bandwidth_limit."),
    (CORE + "gateway.rs", "ModelRoute", "gateway.routes[]."),
    (CORE + "session.rs", "SessionPolicies", "policies."),
    (CORE + "session.rs", "PolicyLayer", "policy_layer."),
    (CORE + "session.rs", "NetworkPolicyLayer", "network_layer."),
]
BUNDLE_GROUPS = [
    ("crates/pvisor/src/runtime/bundle.rs", name, prefix)
    for name, prefix in [
        ("RunBundle", ""), ("BundleRun", "run."), ("SafetySummary", "safety."),
        ("FilesystemSummary", "filesystem."), ("NetworkSummary", "network."),
        ("ResourceSummary", "resources."), ("BundleArtifact", "artifacts[]."),
    ]
] + [
    ("crates/pvisor/src/runtime/registry.rs", "EnvironmentProjection", "environment."),
    ("crates/pvisor/src/runtime/registry.rs", "RunLineage", "lineage."),
    (CORE + "overlay.rs", "ChangeEntry", "filesystem.changes[]."),
]


def fields(source, name):
    match = re.search(r"pub struct " + re.escape(name) + r"\s*\{(.*?)\n\}", source, re.S)
    if not match:
        raise ValueError(f"missing struct {name}")
    attrs, result = [], {}
    for line in match[1].splitlines():
        line = line.strip()
        if not line or line.startswith("//"):
            continue
        if line.startswith("#["):
            attrs.append(line)
            continue
        field = re.fullmatch(r"pub (\w+): (.+),", line)
        if not field:
            raise ValueError(f"unsupported field syntax in {name}: {line}")
        annotations = " ".join(attrs)
        attrs = []
        if re.search(r"#\[serde\(skip\)\]", annotations):
            continue
        renamed = re.search(r'rename\s*=\s*"([^"]+)"', annotations)
        result[renamed[1] if renamed else field[1]] = field[2]
    return result


def inventory(root=ROOT, groups=GROUPS):
    return {
        prefix + field: kind
        for file, name, prefix in groups
        for field, kind in fields((root / file).read_text(), name).items()
    }


def check_surface(root, page_name, marker, groups):
    expected = inventory(root, groups)
    for locale in ("en", "zh"):
        path = root / f"docs/src/{locale}/reference/{page_name}.md"
        page = path.read_text()
        try:
            table = page.split(f"<!-- {marker}:start -->", 1)[1].split(
                f"<!-- {marker}:end -->", 1
            )[0]
        except IndexError:
            raise SystemExit(f"{path}: missing complete field reference") from None
        rows = re.findall(r"^\| `([^`]+)` \| `([^`]+)` \|", table, re.M)
        documented = dict(rows)
        if len(documented) != len(rows):
            raise SystemExit(f"{path}: duplicate config fields")
        if documented != expected:
            missing = sorted(expected.keys() - documented.keys())
            extra = sorted(documented.keys() - expected.keys())
            changed = sorted(k for k in expected.keys() & documented.keys()
                             if expected[k] != documented[k])
            raise SystemExit(f"{path}: {page_name} reference drift: missing={missing}, "
                             f"extra={extra}, changed types={changed}")
    return len(expected)


def check(root=ROOT):
    config_count = check_surface(root, "config", "config-fields", GROUPS)
    check_surface(root, "run-bundle", "bundle-fields", BUNDLE_GROUPS)
    return config_count


if __name__ == "__main__":
    print(f"Checked {check()} configuration fields in both languages")
