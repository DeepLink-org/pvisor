#!/usr/bin/env python3
"""Lossless, line-oriented TSV storage for published benchmark evidence."""

import argparse
import csv
import hashlib
import json
import re
import shutil
import tempfile
from pathlib import Path

HEADER = ("path", "type", "value")
INDEX_HEADER = (
    "source",
    "source_sha256",
    "target",
    "target_sha256",
    "semantic_sha256",
    "source_bytes",
    "target_bytes",
    "rows",
)


def _escape(value):
    # Keep each field on one physical line, including logs and control bytes.
    escaped = json.dumps(value, ensure_ascii=False)[1:-1]
    spaces = len(escaped) - len(escaped.rstrip(" "))
    return escaped[:-spaces] + r"\u0020" * spaces if spaces else escaped


def _unescape(value):
    return json.loads('"' + value + '"')


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"Duplicate JSON object key: {key!r}")
        result[key] = value
    return result


def resolve(path):
    """Prefer the requested format, falling back to the other evidence format."""
    path = Path(path)
    if not path.exists() and path.suffix in (".json", ".tsv"):
        other = path.with_suffix(".tsv" if path.suffix == ".json" else ".json")
        if other.exists():
            return other
    return path


def load(path):
    path = resolve(path)
    if path.suffix == ".json":
        return json.loads(path.read_text(), object_pairs_hook=_unique_object)
    if path.suffix != ".tsv":
        raise ValueError(f"Unsupported evidence format: {path}")
    containers = {}
    seen = set()
    with path.open(newline="") as stream:
        reader = csv.reader(stream, delimiter="\t", quoting=csv.QUOTE_NONE)
        if tuple(next(reader, ())) != HEADER:
            raise ValueError(f"Invalid evidence header: {path}")
        for line, row in enumerate(reader, 2):
            if len(row) != 3:
                raise ValueError(f"{path}:{line}: expected three TSV fields")
            pointer, kind, cell = row
            pointer = _unescape(pointer)
            if pointer and not pointer.startswith("/"):
                raise ValueError(f"{path}:{line}: invalid JSON Pointer")
            if re.search(r"~(?:[^01]|$)", pointer):
                raise ValueError(f"{path}:{line}: invalid JSON Pointer escape")
            if pointer in seen:
                raise ValueError(f"{path}:{line}: duplicate path {pointer!r}")
            seen.add(pointer)
            if kind in ("object", "array"):
                if cell not in ("", "-"):
                    raise ValueError(f"{path}:{line}: invalid container marker")
                value = {} if kind == "object" else []
                containers[pointer] = value
            elif kind == "string":
                value = "" if cell == '""' else _unescape(cell)
            else:
                value = json.loads(cell)
                expected = {"null": type(None), "boolean": bool, "integer": int, "float": float}
                if kind not in expected or type(value) is not expected[kind]:
                    raise ValueError(f"{path}:{line}: value disagrees with type {kind!r}")
            if not pointer:
                if len(seen) != 1:
                    raise ValueError(f"{path}:{line}: root must precede its children")
                root = value
                continue
            parent_pointer, key = pointer.rsplit("/", 1)
            if parent_pointer not in containers:
                raise ValueError(f"{path}:{line}: parent container is missing")
            key = key.replace("~1", "/").replace("~0", "~")
            parent = containers[parent_pointer]
            if isinstance(parent, list):
                if key != str(len(parent)):
                    raise ValueError(f"{path}:{line}: array indexes must be contiguous and ordered")
                parent.append(value)
            else:
                parent[key] = value
    if "" not in seen:
        raise ValueError(f"{path}: root is missing")
    return root


def _rows(value, pointer=""):
    if isinstance(value, dict):
        yield (_escape(pointer), "object", "-")
        for key in sorted(value):
            child = pointer + "/" + key.replace("~", "~0").replace("/", "~1")
            yield from _rows(value[key], child)
    elif isinstance(value, list):
        yield (_escape(pointer), "array", "-")
        for index, item in enumerate(value):
            yield from _rows(item, pointer + "/" + str(index))
    elif isinstance(value, str):
        yield (_escape(pointer), "string", _escape(value) if value else '""')
    else:
        kind = {type(None): "null", bool: "boolean", int: "integer", float: "float"}[type(value)]
        yield (_escape(pointer), kind, json.dumps(value))


def write(path, value):
    """Write deterministic typed TSV; object order is lexical, array order exact."""
    path = Path(path)
    if path.suffix != ".tsv":
        raise ValueError("Published evidence must use the .tsv suffix")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="",
            dir=path.parent,
            prefix=".evidence-",
            delete=False,
        ) as stream:
            temporary = Path(stream.name)
            writer = csv.writer(
                stream, delimiter="\t", quoting=csv.QUOTE_NONE, quotechar=None, lineterminator="\n"
            )
            writer.writerow(HEADER)
            count = 0
            for row in _rows(value):
                writer.writerow(row)
                count += 1
        temporary.chmod(0o644)
        temporary.replace(path)
        return count
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def semantic_sha256(value):
    """Hash canonical JSON values, separately from original/converted file bytes."""
    digest = hashlib.sha256()
    encoder = json.JSONEncoder(sort_keys=True, ensure_ascii=True, separators=(",", ":"))
    for chunk in encoder.iterencode(value):
        digest.update(chunk.encode())
    return digest.hexdigest()


def _file_sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _index(root):
    path = root / "conversion.tsv"
    if not path.exists():
        return []
    with path.open(newline="") as stream:
        reader = csv.DictReader(stream, delimiter="\t")
        if tuple(reader.fieldnames or ()) != INDEX_HEADER:
            raise ValueError(f"Invalid conversion index: {path}")
        return list(reader)


def copy_evidence(source, target):
    """Copy runtime JSON unchanged, or decode a TSV input for a JSON export stage."""
    source, target = resolve(source), Path(target)
    if source.suffix == ".tsv" and target.suffix == ".json":
        target.write_text(json.dumps(load(source), ensure_ascii=False, indent=2) + "\n")
    else:
        shutil.copy2(source, target)


def migrate(root, *, replace=False, keep_json=False):
    """Convert a publication tree; remove sources only after all checks succeed."""
    root = Path(root)
    sources = sorted(root.rglob("*.json"))
    if not sources:
        return 0
    for source in sources:
        if not replace and source.with_suffix(".tsv").exists():
            raise FileExistsError(f"Refusing to overwrite evidence: {source.with_suffix('.tsv')}")
    records = {row["target"]: row for row in _index(root)}
    source_hashes = {}
    for source in sources:
        target = source.with_suffix(".tsv")
        original = source.read_bytes()
        source_hashes[source] = hashlib.sha256(original).hexdigest()
        value = json.loads(original, object_pairs_hook=_unique_object)
        semantic = semantic_sha256(value)
        rows = write(target, value)
        if semantic_sha256(load(target)) != semantic:
            raise ValueError(f"Conversion changed evidence: {source}")
        records[str(target.relative_to(root))] = dict(
            source=str(source.relative_to(root)),
            source_sha256=source_hashes[source],
            target=str(target.relative_to(root)),
            target_sha256=_file_sha256(target),
            semantic_sha256=semantic,
            source_bytes=len(original),
            target_bytes=target.stat().st_size,
            rows=rows,
        )
    for source, original_hash in source_hashes.items():
        if _file_sha256(source) != original_hash:
            raise ValueError(f"Source changed during conversion; sources retained: {source}")
    with (root / "conversion.tsv").open("w", newline="") as stream:
        writer = csv.DictWriter(
            stream, fieldnames=INDEX_HEADER, delimiter="\t", lineterminator="\n"
        )
        writer.writeheader()
        writer.writerows(records[key] for key in sorted(records))
    if not keep_json:
        for source in sources:
            source.unlink()
    return len(sources)


def check(root, *, allow_json=False):
    root = Path(root)
    if not allow_json and any(root.rglob("*.json")):
        raise ValueError("Published benchmark evidence still contains JSON files")
    records = _index(root)
    seen = set()
    for row in records:
        target = root / row["target"]
        if not target.resolve().is_relative_to(root.resolve()) or row["target"] in seen:
            raise ValueError(f"Invalid or duplicate target: {row['target']}")
        seen.add(row["target"])
        if _file_sha256(target) != row["target_sha256"]:
            raise ValueError(f"Converted file hash mismatch: {target}")
        if semantic_sha256(load(target)) != row["semantic_sha256"]:
            raise ValueError(f"Evidence value hash mismatch: {target}")
    return len(records)


def retire(root, backup_dir):
    """Retire indexed JSON only after verifying a retained original-byte backup."""
    root, backup_dir = Path(root), Path(backup_dir)
    if backup_dir.resolve().is_relative_to(root.resolve()):
        raise ValueError("Original-byte backups must be outside the publication tree")
    check(root, allow_json=True)
    records = {row["source"]: row for row in _index(root)}
    sources = sorted(root.rglob("*.json"))
    for source in sources:
        name = str(source.relative_to(root))
        row = records.get(name)
        if row is None or _file_sha256(source) != row["source_sha256"]:
            raise ValueError(f"Unconverted or changed source; retained: {source}")
        backup = backup_dir / name
        backup.parent.mkdir(parents=True, exist_ok=True)
        if not backup.exists():
            with backup.open("xb") as stream, source.open("rb") as original:
                shutil.copyfileobj(original, stream)
        if _file_sha256(backup) != row["source_sha256"]:
            raise ValueError(f"Original-byte backup differs; source retained: {backup}")
    # Verify every backup and current source before removing any publication copy.
    for source in sources:
        row = records[str(source.relative_to(root))]
        if _file_sha256(source) != row["source_sha256"]:
            raise ValueError(f"Source changed during backup; retained: {source}")
    backup_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(root / "conversion.tsv", backup_dir / "conversion.tsv")
    for source in sources:
        source.unlink()
    return len(sources)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    migration = commands.add_parser("migrate")
    migration.add_argument("root", type=Path)
    migration.add_argument("--keep-json", action="store_true")
    checking = commands.add_parser("check")
    checking.add_argument("root", type=Path)
    checking.add_argument("--allow-json", action="store_true")
    retirement = commands.add_parser("retire")
    retirement.add_argument("root", type=Path)
    retirement.add_argument("--backup-dir", type=Path, required=True)
    convert = commands.add_parser("convert")
    convert.add_argument("source", type=Path)
    convert.add_argument("target", type=Path)
    args = parser.parse_args()
    if args.command == "migrate":
        print(f"Converted {migrate(args.root, keep_json=args.keep_json)} JSON files to TSV")
    elif args.command == "check":
        print(f"Verified {check(args.root, allow_json=args.allow_json)} converted evidence files")
    elif args.command == "retire":
        print(
            f"Retired {retire(args.root, args.backup_dir)} JSON publication copies; originals retained in {args.backup_dir}"
        )
    else:
        if args.target.exists():
            raise FileExistsError(f"Refusing to overwrite {args.target}")
        value = load(args.source)
        if args.target.suffix == ".tsv":
            write(args.target, value)
        elif args.target.suffix == ".json":
            args.target.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")
        else:
            raise ValueError("Target must be .tsv or .json")


if __name__ == "__main__":
    main()
