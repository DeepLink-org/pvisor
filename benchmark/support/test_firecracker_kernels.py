"""Offline artifact preparation and provenance gates; no VM or rebuild."""

import json
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from firecracker_kernels import prepare_kernel, verify_kernel_receipt


def prepare(tmp_path, kind, initrd=False):
    config = tmp_path / "source.config"
    config.write_text("CONFIG_VIRTIO_BLK=y\n")
    source = tmp_path / "source.kernel"
    source.write_bytes(b"\x7fELF exact independent bytes")
    provenance = tmp_path / "source.json"
    metadata = {
        "origin": "official-distro-stock" if kind == "fc-system" else "independent-custom",
        "pvisor_firmware": False,
        "source_url": "https://vendor.example/kernel",
        "distribution": "Test distro",
        "package": "linux-image",
        "package_version": "1.2",
        "source_revision": "abc123",
        "build_command": "recorded independent build",
        "compiler": "gcc 14",
    }
    provenance.write_text(json.dumps(metadata))
    kwargs = dict(kind=kind, config=config, provenance=provenance, output=tmp_path / "frozen")
    if kind == "fc-system":
        extractor = tmp_path / "extract-vmlinux"
        extractor.write_text('#!/bin/sh\ncat "$1"\n')
        kwargs.update(vmlinuz=source, extractor=extractor)
    else:
        kwargs["kernel"] = source
    if initrd:
        image = tmp_path / "source.initrd"
        image.write_bytes(b"exact initrd")
        kwargs["initrd"] = image
    return prepare_kernel(**kwargs)


class FirecrackerKernelsTests(unittest.TestCase):
    def test_freeze_exact_bytes_and_verify_independent_sources(self):
        for kind in ["fc-system", "fc-reference"]:
            for initrd in [False, True]:
                with self.subTest(kind=kind, initrd=initrd):
                    with ExitStack() as resources:
                        tmp_path = Path(
                            resources.enter_context(
                                tempfile.TemporaryDirectory(prefix="pvisor-test-")
                            )
                        ).resolve()
                        receipt = prepare(tmp_path, kind, initrd)
                        identity = verify_kernel_receipt(receipt, kind)
                        assert identity["record"]["variant"] == kind
                        assert ("initrd" in identity["paths"]) == initrd
                        assert (
                            receipt.parent / "kernel"
                        ).read_bytes() == b"\x7fELF exact independent bytes"
                        assert ("extractor" in identity["paths"]) == (kind == "fc-system")
                        with self.assertRaises(FileExistsError):
                            prepare(tmp_path, kind, initrd)

    def test_each_stock_artifact_is_hash_gated(self):
        for artifact in ["kernel", "config", "provenance", "vmlinuz", "extractor", "initrd"]:
            with self.subTest(artifact=artifact):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    receipt = prepare(tmp_path, "fc-system", True)
                    (receipt.parent / artifact).write_bytes(b"changed")
                    with self.assertRaisesRegex(ValueError, "hash mismatch"):
                        verify_kernel_receipt(receipt, "fc-system")

    def test_receipt_cannot_relabel_custom_as_stock(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            receipt = prepare(tmp_path, "fc-reference")
            with self.assertRaisesRegex(ValueError, "variant mismatch"):
                verify_kernel_receipt(receipt, "fc-system")
            record = json.loads(receipt.read_text())
            record["variant"] = "fc-system"
            receipt.write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, "non-pvisor origin"):
                verify_kernel_receipt(receipt, "fc-system")

    def test_invalid_stock_provenance_and_artifacts_rejected(self):
        for fault in ["firmware", "missing-package", "symlink", "non-elf"]:
            with self.subTest(fault=fault):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    receipt = prepare(tmp_path, "fc-system")
                    record = json.loads(receipt.read_text())
                    if fault == "firmware":
                        record["provenance"]["pvisor_firmware"] = True
                    elif fault == "missing-package":
                        del record["provenance"]["package"]
                    elif fault == "symlink":
                        config = receipt.parent / "config"
                        config.unlink()
                        config.symlink_to(tmp_path / "source.config")
                    else:
                        from firecracker_kernels import sha256

                        kernel = receipt.parent / "kernel"
                        kernel.write_bytes(b"not ELF")
                        record["artifacts"]["kernel"]["sha256"] = sha256(kernel)
                    receipt.write_text(json.dumps(record))
                    with self.assertRaises(ValueError):
                        verify_kernel_receipt(receipt, "fc-system")
