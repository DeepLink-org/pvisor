//! Identical immutable Python environment and checked tool workload for native gates and experiments.
use crate::native_cache;
use pvisor_cluster::*;
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec};
use std::{collections::BTreeMap, fs, path::Path, process::Command};

pub(crate) fn python_layer(source: &Path, cache: &Path) -> EnvironmentLayer {
    fn copy_python_sources(source: &Path, target: &Path) {
        fs::create_dir_all(target).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if ["site-packages", "__pycache__", "lib-dynload"]
                .iter()
                .any(|s| name == *s)
            {
                continue;
            }
            let metadata = fs::metadata(entry.path()).unwrap();
            if metadata.is_dir() {
                copy_python_sources(&entry.path(), &target.join(&name));
            } else if metadata.is_file() && entry.path().extension().is_some_and(|e| e == "py") {
                fs::copy(entry.path(), target.join(&name)).unwrap();
            }
        }
    }
    let output = Command::new("/usr/bin/python3").args(["-c", "import urllib.request,json,subprocess,pathlib,sys,sysconfig,encodings.idna; print(json.dumps({'stdlib':sysconfig.get_path('stdlib'),'extensions':sorted({m.__file__ for m in sys.modules.values() if (getattr(m,'__file__','') or '').endswith('.so')})}))"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let paths: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    native_cache::publish_layer_prepared(source, cache, "agent-python", &[], false, |root| {
        native_cache::copy_program(root, "/usr/bin/python3");
        let stdlib = Path::new(paths["stdlib"].as_str().unwrap());
        copy_python_sources(stdlib, &root.join(stdlib.strip_prefix("/").unwrap()));
        for extension in paths["extensions"].as_array().unwrap() {
            native_cache::copy_program(root, extension.as_str().unwrap());
        }
    })
}

pub(crate) fn task(id: &str, environment: &str) -> TaskSpec {
    let mut run = RunSpec::process(id, "test-scaffold", "/usr/bin/python3");
    let RunInvocation::Process(process) = &mut run.invocation;
    process.args = vec!["/toolkit/agent.py".into()];
    process.cwd = Some("/env".into());
    process.inherit_env = false;
    process
        .env
        .insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
    process
        .env
        .insert("PVISOR_TEST_BINARY_ARTIFACT".into(), "1".into());
    run.runtime.timeout_ms = Some(30_000);
    run.runtime.max_output_bytes = 8192;
    run.capabilities.models = vec!["test-model".into()];
    TaskSpec {
        retain_artifacts: Some(ArtifactRetention {
            version: ARTIFACT_EXPORT_VERSION,
            trace: true,
            workspace_upper: true,
        }),
        gateway: Some(GatewayRequirement {
            version: CLUSTER_VERSION,
            level: pvisor_core::gateway::CaptureLevel::Dialogue,
            models: vec!["test-model".into()],
        }),
        cpu_qos: None,
        version: CLUSTER_VERSION,
        id: id.into(),
        tenant: "agents".into(),
        run,
        execution: ExecutionClass {
            executor: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
        },
        resources: Resources {
            slots: 1,
            memory_bytes: 256 * 1024 * 1024,
            cpu_millis: 1000,
        },
        labels: BTreeMap::new(),
        cache_keys: vec![],
        retain_bundle: true,
        environment: Some(environment.into()),
        restore: None,
    }
}
