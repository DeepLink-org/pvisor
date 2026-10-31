"""Fail-closed prepared-bundle contract; private image hashing runs in-budget."""

import hashlib
import json
import os
import stat
import sys
from pathlib import Path

from arrival_common import cpus, load, sha

HERE = Path(__file__).resolve().parent


def image_identity(context):
    """Comparable private-image identity without reading data outside its budget.

    Content hashes come from the separately frozen inventory and are rechecked
    inside the condition. Inode numbers are normalized into hardlink groups.
    """
    frozen = load(Path(context["root"]) / "frozen.json")["files"]
    digest = hashlib.sha256()
    links = {}
    for root in sorted(map(Path, context["image_roots"]), key=lambda p: p.name):
        for path in [root, *sorted(root.rglob("*"))]:
            st = path.lstat()
            relative = root.name + "/" + str(path.relative_to(root))
            identity = (st.st_dev, st.st_ino)
            link = links.setdefault(identity, relative) if stat.S_ISREG(st.st_mode) else None
            attrs = {
                name: os.getxattr(path, name, follow_symlinks=False).hex()
                for name in sorted(os.listxattr(path, follow_symlinks=False))
            }
            entry = [
                relative,
                st.st_mode,
                st.st_uid,
                st.st_gid,
                attrs,
                link,
                os.readlink(path) if path.is_symlink() else None,
                frozen[str(path)] if stat.S_ISREG(st.st_mode) else None,
            ]
            digest.update(json.dumps(entry, sort_keys=True).encode())
            digest.update(b"\n")
    return digest.hexdigest()


def verify_metadata(context):
    root = Path(context["root"])
    protocol = load(HERE / "protocol.json")
    manifest = load(root / "manifest.json")
    if manifest["binary_sha256"] != protocol["source_binary_sha"]:
        raise ValueError("Unexpected runtime: historical protocol is binary-pinned")
    if load(root / "advance-progress.json")["status"] != "all-calibration-passed":
        raise ValueError("Strict trace/reference qualification required")
    if not context["image_files"] or not context["image_roots"]:
        raise ValueError("Explicit private image inventory required")
    roots = [Path(p).resolve() for p in context["image_roots"]]
    actual = {
        str(p.resolve()) for r in roots for p in r.rglob("*") if p.is_file() and not p.is_symlink()
    }
    if set(context["image_files"]) != actual:
        raise ValueError("Private image inventory is incomplete")
    for p in map(Path, context["image_files"]):
        if p.resolve() != p or not any(r in p.parents for r in roots):
            raise ValueError("Image escapes private roots or uses symlink")
    frozen = load(root / "frozen.json")["files"]
    required = {
        str(root / p)
        for p in ("manifest.json", "spec.json", "admission-plan.json", "advance-progress.json")
    }
    required |= set(context["image_files"]) | set(manifest["source_files"]) | {manifest["binary"]}
    required |= {
        str(p.resolve())
        for sub in ("configs", "plans")
        for p in (root / sub).rglob("*")
        if p.is_file() and not p.is_symlink()
    }
    required |= {str(p.resolve()) for p in HERE.glob("*.py")} | {str(HERE / "protocol.json")}
    required |= {str(Path(context["context_file"]).resolve())}
    if not required <= set(frozen):
        raise ValueError("Incomplete frozen inputs: " + repr(sorted(required - set(frozen))))
    if not frozen or any(not isinstance(v, str) or len(v) != 64 for v in frozen.values()):
        raise ValueError("Invalid SHA256 inventory")
    # Configs are frozen archive-bridge inputs, not current-main CLI configs.
    for task in ("vuls-666", "nodebb-291", "qutebrowser-667"):
        path = root / "configs" / (task + "-" + context["policy"] + ".json")
        config = load(path)
        if (
            config["cpus"] != 2
            or config["management"] != context["policy"]
            or config["host_http_proxy"] is not None
        ):
            raise ValueError("Frozen VM/model-routing configuration changed")
        if config["binary"] != manifest["binary"]:
            raise ValueError("Config runtime differs from pinned binary")
        if len(config["tasks"]) != 1 or config["tasks"][0]["name"] != task:
            raise ValueError("One exact trace per config required")
        prepared_path = Path(config["tasks"][0]["prepared"])
        if str(prepared_path) not in frozen or Path(load(prepared_path)["rootfs"]) not in roots:
            raise ValueError("Prepared config does not use a private image")
        required_external = [config["require_oracle"]]
        for directory in ("task_dir", "firmware"):
            required_external += [str(p) for p in Path(config[directory]).rglob("*") if p.is_file()]
        trial = Path(manifest["tasks"][task]["source_trial"])
        required_external += [str(p) for p in trial.rglob("*") if p.is_file()]
        if not set(required_external) <= set(frozen):
            raise ValueError("Reference, oracle, task or firmware evidence is not frozen")
    if image_identity(context) != context["image_identity_sha256"]:
        raise ValueError("Private image metadata/content identity changed")
    return frozen


def verify(context_path):
    import os

    from arrival_batch import budget, service_group
    from arrival_common import cgroup_snapshot

    c = load(context_path)
    if Path(c["context_file"]).resolve() != context_path.resolve():
        raise ValueError("Context path mismatch")
    own = Path("/proc/self/cgroup").read_text()
    if "/" + c["slice"] + "/" not in own or os.sched_getaffinity(0) != cpus(c["helper_cpus"]):
        raise ValueError("Run verification/warmup inside its own condition cgroup")
    group = service_group(c["slice"])
    budget(group)
    supervisor = service_group(c["supervisor"])
    if int((supervisor / "memory.max").read_text()) != 256 * 2**20:
        raise ValueError("Coordinator RAM budget changed")
    q, p = map(int, (supervisor / "cpu.max").read_text().split())
    if q * 4 != p or int((supervisor / "memory.swap.max").read_text()) != 0:
        raise ValueError("Coordinator CPU/swap budget changed")
    for path, expected in verify_metadata(c).items():
        if sha(Path(path)) != expected:
            raise ValueError("Frozen input changed: " + path)
    snap = cgroup_snapshot(group)
    if any(snap["memory_events"][k] for k in ("oom", "oom_kill")) or snap["swap_current"]:
        raise ValueError("Preparation or run encountered OOM/swap")
    print(json.dumps({"passed": True, "image_files": len(c["image_files"])}))


if __name__ == "__main__":
    if sys.flags.optimize:
        raise RuntimeError("Assertions must remain enabled")
    verify(Path(sys.argv[1]))
