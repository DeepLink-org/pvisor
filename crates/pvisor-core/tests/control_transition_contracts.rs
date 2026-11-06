use pvisor_core::{
    ControlController, ControlEffect, ControlMachine, ControlReason, ControlRequest, ControlState,
    ControlTransition, ModelAccessPolicy, ModelCallRequest,
};

struct Decision(ControlTransition);

impl ControlController for Decision {
    fn authorize(&self, _: ControlRequest<'_>) -> ControlTransition {
        self.0.clone()
    }
}

#[test]
fn bounded_control_sequences_preserve_authorization_and_terminal_outcomes() {
    // Exhaust every length-five sequence, including retries after errors and
    // terminal states. No external effects are implied by authorization.
    let policy = ModelAccessPolicy::default();
    let request = ModelCallRequest {
        run_id: None,
        execution_id: None,
        call_id: "bounded-control".into(),
        client_model: "test".into(),
        upstream_model: "test".into(),
        provider: "test".into(),
        protocol: "test".into(),
        upstream_host: "unused.invalid".into(),
    };
    for sequence in 0..5_usize.pow(5) {
        let mut remaining = sequence;
        let mut machine = ControlMachine::new();
        let mut expected = ControlState::Requested;
        for _ in 0..5 {
            let action = remaining % 5;
            remaining /= 5;
            let before = machine.history().to_vec();
            let next = match (expected, action) {
                (ControlState::Requested, 0) => Some(ControlState::Allowed),
                (ControlState::Requested, 1) => Some(ControlState::Denied),
                (ControlState::Allowed | ControlState::Denied, 3 | 4) => {
                    let effect = if expected == ControlState::Allowed {
                        ControlEffect::Allow
                    } else {
                        ControlEffect::Deny
                    };
                    Some(if action == 3 {
                        ControlState::Applied { effect }
                    } else {
                        ControlState::Failed { effect }
                    })
                }
                _ => None,
            };
            let result = match action {
                0..=2 => {
                    let decision = match action {
                        0 => ControlTransition::allowed(ControlReason::ModelAllowed),
                        1 => ControlTransition::denied(ControlReason::ModelNotAllowed),
                        _ => ControlTransition {
                            from: ControlState::Allowed,
                            to: ControlState::Applied {
                                effect: ControlEffect::Allow,
                            },
                            reason: ControlReason::ModelAllowed,
                        },
                    };
                    machine.authorize(
                        &Decision(decision),
                        ControlRequest::Model {
                            policy: &policy,
                            request: &request,
                        },
                    )
                }
                3 => machine.applied(),
                _ => machine.failed(),
            };
            assert_eq!(result.is_ok(), next.is_some(), "{sequence}: {action}");
            if let Some(next) = next {
                let transition = result.unwrap();
                assert_eq!(transition.from, expected);
                assert_eq!(transition.to, next);
                assert_eq!(machine.history().len(), before.len() + 1);
                assert_eq!(&machine.history()[..before.len()], before);
                if let Some(decision) = before.last() {
                    assert_eq!(transition.reason, decision.reason);
                }
                assert_eq!(machine.history().last(), Some(&transition));
                expected = next;
            } else {
                assert_eq!(machine.history(), before, "{sequence}: {action}");
            }
            assert_eq!(machine.state(), expected, "{sequence}: {action}");
            // Serialization must not turn a decision into an application receipt.
            let restored: ControlMachine =
                serde_json::from_slice(&serde_json::to_vec(&machine).unwrap()).unwrap();
            assert_eq!(restored.state(), expected);
            assert_eq!(restored.history(), machine.history());
        }
    }
}
