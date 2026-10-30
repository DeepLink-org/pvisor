//! Planner regressions checked through the existing public crate boundary.
use oci_spec::runtime::Spec;
use pvisor_shim::{
    cgroup::control_files,
    plan::{ContainerPlan, IoPlan, build_plan},
};
use serde_json::{Value, json};
use std::path::Path;

fn base() -> Value {
    json!({"ociVersion":"1.0.2", "root":{"path":"rootfs"},
        "process":{"args":["/bin/true"], "cwd":"/", "user":{"uid":0,"gid":0}}})
}

fn plan(value: Value) -> Result<ContainerPlan, pvisor_shim::plan::PlanError> {
    let spec: Spec = serde_json::from_value(value).unwrap();
    build_plan(
        &spec,
        "contract",
        Path::new("/unused-policy-contract"),
        vec![],
        IoPlan::default(),
    )
}

#[test]
fn swap_total_is_converted_without_a_unified_override() {
    for (limit, swap, expected) in [
        (Some(1000), Some(1000), Some("0")),
        (Some(1000), Some(3000), Some("2000")),
        (Some(1000), Some(-1), Some("max")),
        (Some(1000), None, None),
        (Some(1000), Some(0), None),
        (Some(-1), None, Some("max")),
        (Some(-1), Some(0), Some("max")),
        (Some(-1), Some(3000), Some("3000")),
        (None, None, None),
        (None, Some(-1), Some("max")),
        (Some(i64::MAX), Some(i64::MAX), Some("0")),
    ] {
        let mut input = base();
        let mut memory = json!({});
        if let Some(limit) = limit {
            memory["limit"] = json!(limit);
        }
        if let Some(swap) = swap {
            memory["swap"] = json!(swap);
        }
        input["linux"] = json!({"cgroupsPath":"contract", "resources":{"memory":memory}});
        let planned = plan(input).unwrap();
        let files = control_files(planned.cgroup.as_ref().unwrap());
        let actual = files
            .iter()
            .find(|(name, _)| *name == "memory.swap.max")
            .map(|(_, v)| v.as_str());
        assert_eq!(actual, expected, "limit={limit:?}, swap={swap:?}");
    }
}

#[test]
fn invalid_swap_constraints_are_rejected_before_task_creation() {
    for memory in [
        json!({"limit":1000,"swap":999}),
        json!({"swap":3000}),
        json!({"limit":0,"swap":3000}),
        json!({"limit":1000,"swap":-2}),
        json!({"limit":-2,"swap":3000}),
    ] {
        let mut input = base();
        input["linux"] = json!({"resources":{"memory":memory}});
        for unified in [false, true] {
            if unified {
                input["linux"]["resources"]["unified"] = json!({"memory.swap.max":"0"});
            }
            assert!(
                matches!(
                    plan(input.clone()),
                    Err(pvisor_shim::plan::PlanError::InvalidMemoryConstraint(_))
                ),
                "accepted invalid memory contract {memory}, unified={unified}"
            );
        }
    }
}

#[test]
fn unified_swap_overrides_a_valid_oci_conversion() {
    let mut input = base();
    input["linux"] = json!({"cgroupsPath":"contract", "resources":{
        "memory":{"limit":1000,"swap":1000}, "unified":{"memory.swap.max":"123"}}});
    let planned = plan(input).unwrap();
    assert!(
        control_files(planned.cgroup.as_ref().unwrap())
            .contains(&("memory.swap.max", "123".into()))
    );
}

#[test]
fn host_compatibility_markers_do_not_waive_unsupported_constraints() {
    for installer in [false, true] {
        for egress in [false, true] {
            for vm in [false, true] {
                // An egress marker routes to host even with a pod-wide VM annotation.
                let guest_contract = vm && !egress;
                for (field, value) in [
                    ("seccomp", json!({"defaultAction":"SCMP_ACT_ERRNO"})),
                    ("mountLabel", json!("system_u:object_r:container_file_t:s0")),
                    ("apparmorProfile", json!("restricted")),
                    ("selinuxLabel", json!("system_u:system_r:container_t:s0")),
                ] {
                    let mut input = base();
                    input["annotations"] = json!({});
                    if vm {
                        input["annotations"]["io.pvisor.executor"] = json!("vm");
                    }
                    if installer {
                        input["annotations"]["io.kubernetes.cri.container-name"] =
                            json!("execd-installer");
                        input["annotations"]["io.kubernetes.cri.image-name"] =
                            json!("example/execd:latest");
                    }
                    if egress {
                        input["process"]["env"] = json!(["OPENSANDBOX_EGRESS_RULES={}"]);
                    }
                    if matches!(field, "seccomp" | "mountLabel") {
                        input["linux"] = json!({});
                        input["linux"][field] = value;
                    } else {
                        input["process"][field] = value;
                    }
                    let result = plan(input);
                    assert_eq!(
                        result.is_ok(),
                        guest_contract,
                        "installer={installer}, egress={egress}, vm={vm}, field={field}: {result:?}"
                    );
                    if let Ok(planned) = result {
                        assert_eq!(planned.synthetic, installer);
                        assert_eq!(planned.wants_vm(), !installer);
                    }
                }
            }
        }
    }
}

#[test]
fn ordinary_host_installers_and_egress_tasks_still_run() {
    for installer in [false, true] {
        let mut input = base();
        if installer {
            input["annotations"] = json!({"io.kubernetes.cri.container-name":"execd-installer", "io.kubernetes.cri.image-name":"example/execd:latest"});
        } else {
            input["annotations"] = json!({"io.pvisor.executor":"vm"});
            input["process"]["env"] = json!(["OPENSANDBOX_EGRESS_RULES={}"]);
        }
        let planned = plan(input).unwrap();
        assert!(!planned.wants_vm() && !planned.synthetic);
        assert_eq!(planned.process.argv, ["/bin/true"]);
    }
}

#[test]
fn compatibility_markers_cannot_hide_an_invalid_executor() {
    let mut input = base();
    input["annotations"] = json!({"io.pvisor.executor":"typo"});
    input["process"]["env"] = json!(["OPENSANDBOX_EGRESS_RULES={}"]);
    assert!(plan(input).is_err());
}

#[test]
fn host_device_validation_reaches_the_actual_compiler() {
    for device in [
        json!({"allow":true,"type":"c","major":-2,"access":"r"}),
        json!({"allow":true,"type":"b","minor":4294967296_i64,"access":"r"}),
        json!({"allow":true,"type":"a","major":1,"access":"rwm"}),
        json!({"allow":true,"type":"a","access":"r"}),
    ] {
        let mut input = base();
        input["linux"] = json!({"cgroupsPath":"contract", "resources":{"devices":[device]}});
        assert!(
            plan(input).is_err(),
            "accepted invalid device rule {device}"
        );
    }
}

#[test]
fn vm_ignored_host_resources_do_not_create_a_host_cgroup() {
    let mut input = base();
    input["annotations"] = json!({"io.pvisor.executor":"vm"});
    input["linux"] =
        json!({"cgroupsPath":"ignored", "resources":{"memory":{"limit":1000,"swap":999}}});
    let planned = plan(input).unwrap();
    assert!(planned.wants_vm());
    assert!(planned.cgroup.is_none());
}

#[test]
fn vm_network_annotation_rejects_explicit_zero_ports() {
    for target in ["example.com:0", ":443"] {
        let mut input = base();
        input["annotations"] = json!({"io.pvisor.executor":"vm",
            "io.pvisor.vm.network":"allowlist", "io.pvisor.vm.network.allow":target});
        assert!(plan(input).unwrap().vm_network_plan().is_err());
    }
}
