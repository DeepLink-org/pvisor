"""Docker measurements must include child scopes, actual limits and native exit."""

import copy
import json
import tempfile
import unittest
from pathlib import Path

from docker_support import belongs, group_pids, validate_case
from pool_compare import validate_observation
from scanner_compare import save


class DockerEvidenceTests(unittest.TestCase):
    def test_new_task_budget_keeps_memory_cpu_swap_and_interference_gates(self):
        budget = {
            "memory.max": str(2**31),
            "memory.swap.max": "0",
            "memory.swap.current": "0",
            "cpu.max": "400000 100000",
            "pids.max": "256",
        }
        validate_observation([dict(budget=budget)], [dict(jobs=[])], tasks_max=256)
        for key, value in [
            ("memory.max", "max"),
            ("memory.swap.current", "4096"),
            ("cpu.max", "800000 100000"),
            ("pids.max", "128"),
        ]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_observation(
                    [dict(budget={**budget, key: value})], [dict(jobs=[])], tasks_max=256
                )
        with self.assertRaises(ValueError):
            validate_observation(
                [dict(budget=budget)], [dict(jobs=[dict(kind="build/test")])], tasks_max=256
            )
        with self.assertRaises(ValueError):
            validate_observation([dict(budget=budget)], [dict(jobs=[])])  # legacy budget stays 128

    def test_recursive_scope_enumeration_and_prefix_boundary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            group = root / "owned.slice"
            group.mkdir()
            child = group / "docker-one.scope"
            child.mkdir()
            sibling = root / "owned.slice-foreign"
            sibling.mkdir()
            (group / "cgroup.procs").write_text("10\n")
            (child / "cgroup.procs").write_text("20\n21\n")
            (sibling / "cgroup.procs").write_text("99\n")
            self.assertEqual(group_pids(group), [10, 20, 21])
            self.assertTrue(belongs("0::/owned.slice/docker-one.scope\n", "/owned.slice"))
            self.assertFalse(belongs("0::/owned.slice-foreign\n", "/owned.slice"))
            self.assertFalse(belongs("0::/owned.slice\n0::/foreign\n", "/owned.slice"))

    def test_docker_identity_runtime_leaf_limits_and_native_exit_are_required(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inputs = root / "inputs"
            inputs.mkdir()
            save(inputs / "receipt.json", dict(image_id="sha256:frozen"))
            cfg = dict(
                docker_inputs=str(inputs),
                accounting_group="/sys/fs/cgroup/owned.slice",
                docker_slice="owned.slice",
            )
            for n in range(1, 5):
                item = dict(
                    Id=str(n),
                    Image="sha256:frozen",
                    State=dict(Running=True, Pid=n),
                    Config=dict(Cmd=["/memory-init", "/worker"]),
                    HostConfig=dict(
                        CgroupParent="owned.slice",
                        Memory=256 * 1024**2,
                        MemorySwap=256 * 1024**2,
                        NanoCpus=10**9,
                        PidsLimit=16,
                        ReadonlyRootfs=True,
                        NetworkMode="none",
                        CapDrop=["ALL"],
                        SecurityOpt=["no-new-privileges"],
                    ),
                )
                save(root / f"docker-{n}-created.json", item)
                save(
                    root / f"docker-{n}-exit.json",
                    dict(Id=str(n), State=dict(Running=False, OOMKilled=False, ExitCode=0)),
                )
            save(root / "docker-cleanup.json", dict(errors=[]))
            limits = {
                "memory.max": str(256 * 1024**2),
                "memory.swap.max": "0",
                "memory.swap.current": "0",
                "cpu.max": "100000 100000",
                "pids.max": "16",
            }
            processes = [
                dict(pid=n, comm="worker", container_limits=limits.copy()) for n in range(1, 5)
            ]
            processes += [dict(pid=10, comm="dockerd"), dict(pid=11, comm="containerd")]
            phase = dict(
                name="idle-60",
                cgroup=cfg["accounting_group"],
                guest_pids=[1, 2, 3, 4],
                processes=processes,
            )
            raw = dict(
                docker_image_id="sha256:frozen",
                docker_info=dict(
                    CgroupDriver="systemd",
                    CgroupVersion="2",
                    Driver="overlayfs",
                    SecurityOptions=["name=rootless"],
                ),
                phases=[phase],
                before=dict(name="before", cgroup=cfg["accounting_group"]),
                after=dict(name="after", cgroup=cfg["accounting_group"]),
            )
            validate_case(raw, cfg, root)
            for failure in ("image", "runtime", "pid", "memory", "swap", "cpu", "scope"):
                with self.subTest(failure=failure):
                    bad = copy.deepcopy(raw)
                    p = bad["phases"][0]
                    if failure == "image":
                        bad["docker_image_id"] = "sha256:other"
                    if failure == "runtime":
                        p["processes"].pop()
                    if failure == "pid":
                        p["guest_pids"][0] = 99
                    if failure == "memory":
                        p["processes"][0]["container_limits"]["memory.max"] = "max"
                    if failure == "swap":
                        p["processes"][0]["container_limits"]["memory.swap.current"] = "4096"
                    if failure == "cpu":
                        p["processes"][0]["container_limits"]["cpu.max"] = "max 100000"
                    if failure == "scope":
                        p["cgroup"] += "/controller.service"
                    with self.assertRaises(ValueError):
                        validate_case(bad, cfg, root)
            exit_path = root / "docker-1-exit.json"
            exited = json.loads(exit_path.read_text())
            exited["State"]["OOMKilled"] = True
            save(exit_path, exited)
            with self.assertRaisesRegex(ValueError, "exit"):
                validate_case(raw, cfg, root)


if __name__ == "__main__":
    unittest.main()
