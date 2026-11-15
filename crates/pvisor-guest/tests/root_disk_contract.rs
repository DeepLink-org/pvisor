use pvisor_guest::{GuestConfig, ImageLayer, ProcessConfig, RootDisk, StdinMode};

fn controlled() -> GuestConfig {
    GuestConfig {
        argv: vec!["/bin/echo".into(), "hello".into()],
        cwd: "/".into(),
        process: Some(ProcessConfig {
            new_session: true,
            stdin: StdinMode::Closed,
            capabilities: Some(vec![0, 1, 3, 4, 5, 6, 7, 8, 10, 13, 18, 29, 31]),
            ..Default::default()
        }),
        serial_control: true,
        root_disk: Some(RootDisk {
            device: "/dev/vda".into(),
            layers: vec![ImageLayer {
                sha256: "a".repeat(64),
                media_type: "application/vnd.oci.image.layer.v1.tar+gzip".into(),
            }],
        }),
        ..Default::default()
    }
}

#[test]
fn disk_and_serial_contract_roundtrip_uses_linux_paths_on_every_host() {
    let config = controlled();
    config.command().unwrap();
    let bytes = serde_json::to_vec(&config).unwrap();
    let restored: GuestConfig = serde_json::from_slice(&bytes).unwrap();
    assert!(restored.serial_control && !restored.control);
    assert_eq!(restored.root_disk.unwrap().device, "/dev/vda");
    let old = GuestConfig::default();
    let value = serde_json::to_value(old).unwrap();
    assert!(value.get("serial_control").is_none() && value.get("root_disk").is_none());
}

#[test]
fn privileged_or_implicit_serial_input_is_rejected_but_owned_ptys_are_supported() {
    for capability in [2, 16, 17, 19, 21, 27] {
        let mut config = controlled();
        config
            .process
            .as_mut()
            .unwrap()
            .capabilities
            .as_mut()
            .unwrap()
            .push(capability);
        assert!(config.command().is_err(), "{capability}");
    }
    let mut config = controlled();
    config.process.as_mut().unwrap().capabilities = None;
    assert!(config.command().is_err());
    let mut config = controlled();
    config.process.as_mut().unwrap().stdin = StdinMode::Pipe;
    config.process.as_mut().unwrap().tty = Some([24, 80]);
    assert!(
        config.command().is_ok(),
        "restricted serial launches own real PTY input"
    );
    config.process.as_mut().unwrap().stdin = StdinMode::Inherit;
    assert!(config.command().is_err());
    let mut config = controlled();
    config.control = true;
    assert!(config.command().is_err());
}

#[test]
fn malformed_disk_geometry_and_layer_references_fail_before_io() {
    for device in [
        "C:\\disk",
        "/dev/vda/../vdb",
        "/dev/sda",
        "/dev/vdA",
        "/dev/vd",
    ] {
        let mut config = controlled();
        config.root_disk.as_mut().unwrap().device = device.into();
        assert!(config.command().is_err(), "{device}");
    }
    for digest in ["../escape".into(), "A".repeat(64), "a".repeat(63)] {
        let mut config = controlled();
        config.root_disk.as_mut().unwrap().layers[0].sha256 = digest;
        assert!(config.command().is_err());
    }
    let mut config = controlled();
    config.root_disk.as_mut().unwrap().layers[0].media_type = "unknown".into();
    assert!(config.command().is_err());
}
