"""Discover native Scrut inputs and declare feature-level Linux capabilities."""

import argparse
import json
from pathlib import Path


def discover(root):
    """New case files join their owner's job without a testcase allowlist."""
    features = sorted(
        (p.name for p in root.iterdir() if p.is_dir() and any(p.glob("S-*.md"))),
        key=lambda feature: (feature != "vm", feature),
    )
    if not features:
        raise ValueError("empty product feature inventory")
    return [
        {
            "feature": feature,
            "userns": feature
            in {
                "checkpoint",
                "container",
                "gateway",
                "isolation",
                "network",
                "resources",
                "stage",
                "vm",
            },
            "fuse": feature in {"checkpoint", "gateway", "isolation", "resources", "stage", "vm"},
            "kvm": feature in {"network", "vm"},
            "oci": feature == "container",
            "vm_sdk": feature == "vm",
        }
        for feature in features
    ]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--spec-root", type=Path, default=Path("tests/spec"))
    args = parser.parse_args()
    print(json.dumps({"include": discover(args.spec_root)}))
