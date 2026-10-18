"""Publishing must bind user results to their role, execution and raw bytes."""

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "publish_user_workloads", Path(__file__).with_name("publish_user_workloads.py")
)
PUBLISH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PUBLISH)


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value) + "\n")


class PublicationEvidenceTests(unittest.TestCase):
    def temporary_batch(self):
        temporary = tempfile.TemporaryDirectory(prefix="user-publication-synthetic-")
        self.addCleanup(temporary.cleanup)
        batch = Path(temporary.name) / ".data" / "batch"
        batch.mkdir(parents=True)
        return batch

    def test_engineering_input_cannot_publish_as_user_result(self):
        batch = self.temporary_batch()
        write(batch / "protocol.json", dict(benchmark_id="B-FS-ENG", role="engineering A/B"))
        write(batch / "records.json", [])
        with self.assertRaisesRegex(ValueError, "user-facing cohort only"):
            PUBLISH.validate(batch, batch / "analysis")

    def execution(self):
        batch = self.temporary_batch()
        command = f"/usr/bin/python3 {batch}/harness/user_workloads.py --measure {batch}"
        value = dict(trials=1, failed=1)
        write(batch / "in.json", {"filesystem-user-layered": "$ " + command})
        write(batch / "in.render.json", {"filesystem-user-layered": value})
        log = batch / "in.bench.logs/0001.log"
        write(log, value)
        bench = dict(
            format=1,
            template=str(batch / "in.json"),
            cwd=str(batch),
            commands=[
                dict(command=command, kind="script-row", raw=value, value=value, log=str(log))
            ],
            rows=[dict(row="filesystem-user-layered", command=command)],
        )
        write(batch / "in.bench.json", bench)
        return batch, bench

    def test_failed_environment_is_kept_in_actual_execution_result(self):
        batch, _ = self.execution()
        PUBLISH.validate_execution(batch, dict(trials=1, failed=1, excluded=0), [dict(valid=False)])

    def test_semspec_success_cannot_replace_failure_counts(self):
        batch, bench = self.execution()
        bench["commands"][0]["value"] = dict(trials=1, failed=0)
        write(batch / "in.bench.json", bench)
        with self.assertRaisesRegex(ValueError, "does not match"):
            PUBLISH.validate_execution(
                batch, dict(trials=1, failed=1, excluded=0), [dict(valid=False)]
            )

    def test_missing_planned_modes_cannot_reduce_expected_inventory(self):
        batch = self.temporary_batch()
        with self.assertRaisesRegex(ValueError, "full case/mode/environment inventory"):
            PUBLISH.validate_records(
                batch, dict(orders=[dict(case_id="M01", pair=1, backends=["native"])]), []
            )

    def test_reference_schedule_cannot_borrow_the_five_mode_schedule(self):
        batch = self.temporary_batch()
        protocol = dict(
            backends=[dict(id=backend) for backend in PUBLISH.REFERENCE_BACKENDS],
            orders=[
                dict(case_id=case, pair=pair, backends=PUBLISH.BACKENDS)
                for case in PUBLISH.CASES
                for pair in (1, 2, 3)
            ],
        )
        with self.assertRaisesRegex(ValueError, "full case/mode/environment inventory"):
            PUBLISH.validate_records(batch, protocol, [])

    def test_reference_configuration_cannot_omit_a_runtime(self):
        protocol = dict(
            backends=[dict(id=backend) for backend in PUBLISH.REFERENCE_BACKENDS],
            reference_configuration={
                backend: dict(storage="synthetic") for backend in PUBLISH.REFERENCES[:-1]
            },
        )
        with self.assertRaisesRegex(ValueError, "every reference runtime"):
            PUBLISH.validate_references(protocol)

    def test_public_reference_summary_keeps_input_hashes_and_omits_private_paths(self):
        batch = self.temporary_batch()
        private = "/private-host-reference-marker"
        root = batch / "reference-assets/vm"
        artifacts = {
            "reference-assets/vm/kernel/vmlinux": "a" * 64,
            "reference-assets/vm/kernel/kernel-receipt.json": "b" * 64,
            "reference-assets/vm/root.ext4": "c" * 64,
            "reference-assets/vm/init.c": "d" * 64,
            "reference-assets/vm/init": "e" * 64,
        }
        references = dict(
            docker=dict(
                docker_host="unix://" + private + "/docker.sock",
                docker_binary=private + "/docker",
                budget_cgroup=private + "/cgroup",
                budget_slice_unit=private + "/slice",
                setup_receipt=private + "/setup.json",
                setup_sha256="f" * 64,
                storage_driver="overlay2",
                runtime_sha256="1" * 64,
                runtime_source_sha256="2" * 64,
                actual_parent_limits={
                    "cpu.max": "200000 100000",
                    "memory.max": str(2 << 30),
                    "memory.swap.max": "0",
                },
            ),
            qemu=dict(
                kernel=dict(
                    origin="official-distro-stock",
                    package_version="1.2.3",
                    source_url="https://example.invalid/kernel",
                    local_build_path=private,
                ),
                runtime=dict(
                    source=private + "/qemu",
                    executable=private + "/frozen-qemu",
                    sha256="0" * 64,
                    version="QEMU test",
                ),
                boot_firmware=dict(
                    path=private + "/qboot.rom",
                    source=private + "/distribution/qboot.rom",
                    sha256="3" * 64,
                ),
                kernel_receipt=str(root / "kernel/kernel-receipt.json"),
                device_cache="QEMU cache=writeback with flush enabled",
                assets=dict(
                    root_disk=str(root / "root.ext4"),
                    work_disk=private + "/work.ext4",
                    root_disk_bytes=8 << 30,
                    artifacts=artifacts,
                    init_build_command=[private + "/gcc", "-o", private + "/init"],
                    kernel_paths=dict(kernel=str(root / "kernel/vmlinux")),
                ),
            ),
        )
        public = PUBLISH.public_reference_configuration(references, batch)
        self.assertNotIn(private, json.dumps(public))
        self.assertNotIn(str(batch), json.dumps(public))
        self.assertNotIn("artifacts", public["qemu"])
        self.assertEqual(public["qemu"]["runtime"], dict(sha256="0" * 64, version="QEMU test"))
        self.assertEqual(public["qemu"]["boot_firmware"], dict(name="qboot.rom", sha256="3" * 64))
        self.assertEqual(public["qemu"]["assets_summary"]["kernel_sha256"], dict(kernel="a" * 64))
        self.assertEqual(public["qemu"]["assets_summary"]["disk_sha256"], dict(root_disk="c" * 64))
        self.assertEqual(public["qemu"]["assets_summary"]["kernel_receipt_sha256"], "b" * 64)
        self.assertEqual(
            public["qemu"]["assets_summary"]["minimal_init_sha256"],
            {"init.c": "d" * 64, "init": "e" * 64},
        )
        self.assertEqual(public["qemu"]["assets_summary"]["manifest"]["files"], 5)
        self.assertEqual(public["qemu"]["device_cache"], references["qemu"]["device_cache"])
        self.assertEqual(
            public["docker"]["actual_parent_limits"], references["docker"]["actual_parent_limits"]
        )
        self.assertEqual(public["docker"]["setup_sha256"], "f" * 64)
        self.assertNotIn("runtime_sha256", public["docker"])
        self.assertEqual(public["docker"]["launcher_sha256"], "1" * 64)
        self.assertEqual(public["docker"]["launcher_source_sha256"], "2" * 64)

    def test_public_docker_versions_omit_context_and_component_details(self):
        version = dict(
            Client=dict(
                Version="1.2",
                ApiVersion="1.0",
                Os="linux",
                Arch="amd64",
                Context="/private/client-context",
                DockerPath="/private/docker",
            ),
            Server=dict(
                Version="1.3",
                MinAPIVersion="1.0",
                ApiVersion="1.1",
                Os="linux",
                Arch="amd64",
                Components=[dict(Name="runc", Version="1.4", Details=dict(path="/private/runc"))],
            ),
        )
        for value in (version, json.dumps(version)):
            public = PUBLISH.public_reference_runtime_observation(
                dict(docker_version=value, command=["/private/docker"], environment="/private/env")
            )
            self.assertNotIn("/private", json.dumps(public))
            self.assertEqual(public["docker_version"]["Client"]["Version"], "1.2")
            self.assertEqual(
                public["docker_version"]["Server"]["Components"], [dict(Name="runc", Version="1.4")]
            )

    def test_stale_raw_hash_is_rejected_even_if_observations_equal(self):
        batch = self.temporary_batch()
        cases = [
            dict(
                id=case,
                name=case,
                layer="mechanism"
                if case[0] == "M"
                else "resource"
                if case[0] == "R"
                else "scenario",
                parameters={},
            )
            for case in PUBLISH.CASES
        ]
        protocol = dict(
            benchmark_id="B-FS-TOOLS",
            role="user-facing",
            samples=3,
            rounds=3,
            sample_index_base=1,
            catalog=cases,
            cases=PUBLISH.CASES,
            backends=[dict(id=backend) for backend in PUBLISH.BACKENDS],
            artifacts={
                "harness/user_workloads_analysis.py": PUBLISH.sha(
                    PUBLISH.HERE / "user_workloads_analysis.py"
                )
            },
        )
        write(batch / "protocol.json", protocol)
        write(batch / "records.json", [])
        PUBLISH.ANALYSIS.analyze(batch, plots=False)
        write(
            batch / "status.json",
            dict(
                benchmark_id="B-FS-TOOLS",
                role="user-facing",
                trials=0,
                failed=0,
                excluded=0,
                measurements=0,
                complete=True,
            ),
        )
        with (batch / "records.json").open("a") as stream:
            stream.write("\n")
        with (
            mock.patch.object(PUBLISH, "validate_inputs", return_value=({}, {}, {})),
            mock.patch.object(PUBLISH, "validate_execution"),
            mock.patch.object(PUBLISH, "validate_records"),
        ):
            with self.assertRaisesRegex(ValueError, "stale"):
                PUBLISH.validate(batch, batch / "analysis")

    def publication(self, modes):
        batch = self.temporary_batch()
        directory = batch / "analysis"
        directory.mkdir()
        for filename in (
            "protocol.json",
            "records.json",
            "status.json",
            "in.json",
            "in.bench.json",
            "in.render.json",
            "fixture-manifest.json",
            "toolroot-receipt.json",
            "cli-build-receipt.json",
            "firmware-source.json",
        ):
            write(batch / filename, {})
        write(
            batch / "records.json",
            [
                dict(backend="vm", valid=True, ready_record=dict(actual_guest_memtotal_kib=459312)),
                dict(backend="vm-writeback", valid=False),
            ],
        )
        write(batch / "status.json", dict(trials=2, failed=1, skipped=2))
        write(batch / "skipped.json", [dict(reason="synthetic prospective skip")] * 2)
        write(
            batch / "host-observation.json",
            dict(
                phase="during formal sampling",
                observed_unix=1,
                cpu_model="synthetic CPU",
                uname="synthetic kernel",
                local_path="/private/host",
            ),
        )
        write(
            batch / "reference-runtime-observation.json",
            dict(
                docker_version=dict(
                    Client=dict(Version="synthetic client", Context="/private/context"),
                    Server=dict(Version="synthetic server"),
                )
            ),
        )
        write(directory / "summary.json", {})
        for name in ("payload", "libkrunfw.so"):
            path = batch / "bin" / name
            path.parent.mkdir(exist_ok=True)
            path.write_bytes(b"isolated synthetic fixture")
        for table in PUBLISH.TABLES:
            (directory / (table + ".csv")).write_bytes(b"case_id,backend,n\r\nM01,native,3\r\n")
        for figure in PUBLISH.FIGURES:
            (directory / figure).write_bytes(
                b"%PDF-1.4" if figure.endswith(".pdf") else b"<svg></svg>"
            )
        protocol = dict(
            backends=[dict(id=backend) for backend in modes],
            reference_configuration={
                backend: dict(kernel="stock-test-kernel", storage="ext4 block")
                for backend in PUBLISH.REFERENCES
            }
            if modes == PUBLISH.REFERENCE_BACKENDS
            else None,
            budget=dict(guest_mib=512),
            resource_scale="fixed reference",
            timing="separate task clocks",
            cache="three resident rounds",
            trial_timeout_seconds=90,
            skip_remaining_on_case_failure=True,
            failures="prospective skip after first failed cell observation",
            host=dict(
                platform="synthetic host",
                filesystem=json.dumps(
                    dict(
                        filesystems=[
                            dict(
                                target="/private/batch",
                                source="/dev/private",
                                fstype="btrfs",
                                options="rw,compress=zstd:1",
                            )
                        ]
                    )
                ),
            ),
            prepared_tools={},
            artifacts={
                name: "a" * 64
                for name in ("harness/workloads_v2_guest.rs", "harness/workloads_v2_scenarios.rs")
            },
        )
        summary = dict(
            counts=dict(complete_cells=1),
            complete_cells=[dict(case_id="M01", backend="native")],
            statistical_scope={},
        )
        bound = dict(protocol_sha256="b" * 64, records_sha256="c" * 64, analyzer_sha256="d" * 64)
        tool = dict(installed=[], runtime_dependencies=[])
        source = dict(head="synthetic-unit-test", source_hashes={}, artifacts={})
        destination = batch.parent / "published"
        prefix = PUBLISH.REFERENCE_PREFIX if modes == PUBLISH.REFERENCE_BACKENDS else PUBLISH.PREFIX
        with mock.patch.object(
            PUBLISH, "validate", return_value=(protocol, summary, bound, tool, {}, source, False)
        ):
            PUBLISH.publish(batch, destination_root=destination, check=True)
            self.assertFalse(destination.exists())
            self.assertFalse((batch / "publication").exists())
            receipt = PUBLISH.publish(batch, destination_root=destination)
            self.assertEqual(receipt["state"], "partial")
            self.assertEqual(receipt["files"], 8 if prefix == PUBLISH.REFERENCE_PREFIX else 4)
            archive = batch / "publication" / prefix
            self.assertEqual(receipt["archive_files"], 14)
            provenance = json.loads((archive / "provenance.json").read_text())
            public_provenance = (
                destination / f"docs/overrides/assets/benchmarks/{prefix}/provenance.json"
            )
            if prefix == PUBLISH.REFERENCE_PREFIX:
                self.assertEqual(
                    public_provenance.read_bytes(), (archive / "provenance.json").read_bytes()
                )
            else:
                self.assertFalse(public_provenance.exists())
            self.assertEqual(provenance["execution"]["attempted"], 2)
            self.assertEqual(provenance["execution"]["valid"], 1)
            self.assertEqual(provenance["execution"]["failed"], 1)
            self.assertEqual(provenance["execution"]["skipped"], 2)
            self.assertEqual(provenance["execution"]["trial_timeout_seconds"], 90)
            self.assertEqual(
                provenance["inputs"]["skipped.json"], PUBLISH.sha(batch / "skipped.json")
            )
            self.assertEqual(
                provenance["inputs"]["reference-runtime-observation.json"],
                PUBLISH.sha(batch / "reference-runtime-observation.json"),
            )
            self.assertEqual(
                provenance["reference_runtime_observation"]["docker_version"]["Server"]["Version"],
                "synthetic server",
            )
            self.assertEqual(provenance["host_observation"]["cpu_model"], "synthetic CPU")
            self.assertNotIn("/private", json.dumps(provenance["host_observation"]))
            self.assertNotIn("/private", json.dumps(provenance["reference_runtime_observation"]))
            self.assertEqual(
                provenance["observed_guest_memory"]["vm"]["observed_ready_memtotal_kib"], [459312]
            )
            self.assertEqual(
                provenance["observed_guest_memory"]["vm"]["configured_guest_memory_mib"], 512
            )
            self.assertEqual(
                provenance["host"]["filesystems"],
                [dict(fstype="btrfs", options="rw,compress=zstd:1")],
            )
            self.assertEqual(
                provenance["reference_configuration"], protocol["reference_configuration"]
            )
            self.assertNotIn("frozen_artifacts", provenance)
            self.assertEqual(
                provenance["frozen_artifact_manifest"],
                PUBLISH.manifest_summary(protocol["artifacts"]),
            )
            for table, suffix in PUBLISH.TABLES.items():
                target = PUBLISH.csv_path(destination, f"{prefix}-{suffix}.csv")
                archived = archive / f"{prefix}-{suffix}.csv"
                self.assertEqual(archived.read_bytes(), (directory / (table + ".csv")).read_bytes())
                if prefix == PUBLISH.REFERENCE_PREFIX and suffix in ("levels", "ready", "failures"):
                    self.assertEqual(target.read_bytes(), archived.read_bytes())
                    self.assertEqual(
                        provenance["outputs"][str(target.relative_to(destination))],
                        PUBLISH.sha(target),
                    )
                else:
                    self.assertFalse(target.exists())
            self.assertFalse((destination / "docs/src").exists())
            self.assertFalse(list(destination.rglob("*.pdf")))
            self.assertEqual(len(list(destination.rglob("*.svg"))), 4)
            for figure in PUBLISH.FIGURES:
                self.assertEqual(
                    (directory / figure).read_bytes(),
                    b"%PDF-1.4" if figure.endswith(".pdf") else b"<svg></svg>",
                )
            with self.assertRaisesRegex(ValueError, "refusing to overwrite"):
                PUBLISH.publish(batch, destination_root=destination)
        return destination

    def test_five_mode_publication_archives_tables_and_provenance_locally(self):
        self.publication(PUBLISH.BACKENDS)

    def test_reference_publication_has_separate_provenance_and_csv_paths(self):
        destination = self.publication(PUBLISH.REFERENCE_BACKENDS)
        self.assertFalse(
            (destination / "docs/overrides/assets/benchmarks/filesystem-layered").exists()
        )
        provenance = json.loads(
            (
                destination
                / f"docs/overrides/assets/benchmarks/{PUBLISH.REFERENCE_PREFIX}/provenance.json"
            ).read_text()
        )
        self.assertEqual(
            set(provenance["observed_guest_memory"]),
            {"vm", "vm-writeback", "fc-system", "qemu", "qemu-microvm"},
        )
        self.assertTrue(any("storage paths" in value for value in provenance["limitations"]))


if __name__ == "__main__":
    unittest.main()
