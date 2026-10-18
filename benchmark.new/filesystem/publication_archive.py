"""Private derived-output archives; measurement inputs are never overwritten."""

from pathlib import Path


def archive_targets(batch, namespace, files, provenance):
    """Describe one input batch's local publication archive without writing it."""
    if Path(namespace).name != namespace or namespace in ("", ".", ".."):
        raise ValueError("archive namespace must be a plain directory name")
    directory = Path(batch).resolve() / "publication" / namespace
    result = {}
    for name, data in files.items():
        if Path(name).name != name or name in ("", ".", "..", "provenance.json"):
            raise ValueError("archive output requires a plain, unique filename")
        result[directory / name] = data
    result[directory / "provenance.json"] = provenance
    return result


def preflight_archive(targets):
    """Accept only new paths or identical archived bytes, without creating paths."""
    for path, data in targets.items():
        if any(parent.is_symlink() for parent in (path, *path.parents)):
            raise ValueError("archive destination traverses a symlink: " + str(path))
        if path.exists() and (not path.is_file() or path.read_bytes() != data):
            raise ValueError("refusing to overwrite different archived bytes: " + str(path))


def write_archive(targets):
    """Create checked archive files exclusively; preserve existing identical bytes."""
    preflight_archive(targets)
    for path, data in targets.items():
        if path.exists():
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        try:
            with path.open("xb") as stream:
                stream.write(data)
        except FileExistsError:
            if path.is_symlink() or not path.is_file() or path.read_bytes() != data:
                raise ValueError("concurrent archive contains different bytes: " + str(path))
