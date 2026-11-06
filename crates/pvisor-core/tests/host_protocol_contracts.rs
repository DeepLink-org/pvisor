use pvisor_core::host_protocol::{
    AGENTCTL_HOST_VERSION, AgentCtlHostRequest, HostSupervisorCommand, HostSupervisorRequest,
};
use serde_json::json;

#[test]
fn supervisor_envelopes_reject_unexpected_command_fields() {
    let mut seed = 0xc3_2026_u32;
    for _ in 0..16 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        for (operation, expected) in [
            ("inspect", HostSupervisorCommand::Inspect),
            ("pause", HostSupervisorCommand::Pause),
            ("resume", HostSupervisorCommand::Resume),
            ("terminate", HostSupervisorCommand::Terminate),
        ] {
            let wire = json!({
                "version": AGENTCTL_HOST_VERSION,
                "request_id": format!("request-{seed:x}"),
                "target": {
                    "session_id": "session", "execution_id": "execution", "generation": "generation"
                },
                "command": {
                    "auth": {"owner": "owner", "token": "synthetic-test-only"},
                    "operation": {"operation": operation}
                }
            });
            let request: AgentCtlHostRequest<HostSupervisorRequest> =
                serde_json::from_slice(&serde_json::to_vec(&wire).unwrap()).unwrap();
            request.validate().unwrap();
            assert_eq!(request.command.operation, expected);
            assert_eq!(serde_json::to_value(&request).unwrap(), wire);
            for extra in ["port".to_owned(), format!("unexpected_{seed:x}")] {
                let mut invalid = wire.clone();
                invalid["command"]["operation"][&extra] = json!(seed);
                assert!(
                    serde_json::from_slice::<AgentCtlHostRequest<HostSupervisorRequest>>(
                        &serde_json::to_vec(&invalid).unwrap()
                    )
                    .is_err(),
                    "{operation} ignored unexpected field {extra}"
                );
            }
        }
        let endpoint = HostSupervisorCommand::Endpoint {
            port: (1 + seed % u16::MAX as u32) as u16,
        };
        let mut wire = serde_json::to_value(endpoint).unwrap();
        assert_eq!(
            serde_json::from_value::<HostSupervisorCommand>(wire.clone()).unwrap(),
            endpoint
        );
        wire["extra"] = json!(null);
        assert!(serde_json::from_value::<HostSupervisorCommand>(wire).is_err());
    }
}
