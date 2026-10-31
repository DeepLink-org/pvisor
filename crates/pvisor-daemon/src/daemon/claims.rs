//! Executor claims are retained permanently, including sanitized Released tombstones.
//! The existing 16 MiB registry budget bounds retention and rejects new reservations
//! with 429 rather than evicting replay fences. These records are not feature gated.
//! Immutable create results retain public Sandbox metadata/argv/image for exact
//! replay even after cleanup; callers must not put secrets in metadata. Environment
//! and endpoint credentials are not part of that result. Admission precharges two
//! serialized records plus 8 KiB for bounded lifecycle/error fields; the charge is
//! permanent, phase-independent and checked on every commit and reopen.
use super::models::{ClaimIdentity, ClaimPhase};
use super::{ApiError, Record, Sandbox, SandboxStatus};
use chrono::Utc;
#[cfg(any(test, feature = "cluster"))]
use {
    super::models::{ClaimRecord, ClaimView},
    super::{CreateRequest, Daemon},
    crate::runtime::RuntimeSpec,
    chrono::Duration,
    sha2::{Digest, Sha256},
    std::sync::Arc,
};

pub(super) fn validate_identity(identity: &ClaimIdentity) -> Result<(), ApiError> {
    if uuid::Uuid::parse_str(&identity.claim_id)
        .ok()
        .is_none_or(|id| id.to_string() != identity.claim_id)
        || identity.home.is_empty()
        || identity.home.len() > 2048
        || identity.task_id.is_empty()
        || identity.task_id.len() > 2048
    {
        return Err(ApiError::bad_request(
            "claim requires canonical UUID and bounded nonempty home/task identity",
        ));
    }
    Ok(())
}

fn conflict() -> ApiError {
    ApiError::new(
        409,
        "CLAIM_MISMATCH",
        "claim identity or request does not match durable reservation",
    )
}

pub(super) fn require_visible(record: &Record) -> Result<(), ApiError> {
    if record.publicly_visible() {
        Ok(())
    } else {
        Err(ApiError::new(
            404,
            "SANDBOX_NOT_FOUND",
            "sandbox is not publicly available",
        ))
    }
}

pub(super) fn reserve_replay(existing: &Record, proposed: &Record) -> Result<Sandbox, ApiError> {
    match (&existing.claim, &proposed.claim) {
        (Some(old), Some(new))
            if old.identity == new.identity && old.request_digest == new.request_digest =>
        {
            if old.phase == ClaimPhase::Released {
                Err(old.error.clone().unwrap_or_else(released_error))
            } else {
                Ok(existing.sandbox.clone())
            }
        }
        _ => Err(conflict()),
    }
}

fn released_error() -> ApiError {
    ApiError::new(
        409,
        "CLAIM_RELEASED",
        "claim permanently released; old grants cannot relaunch",
    )
}

pub(super) fn tombstone(record: &mut Record) {
    let claim = record.claim.as_mut().expect("claim tombstone");
    if claim.creation_result.is_none() {
        claim.creation_result = Some(Err(ApiError::new(
            409,
            "CLAIM_RELEASED",
            "claim released without a confirmed successful creation",
        )));
    }
    claim.phase = ClaimPhase::Released;
    if claim.error.is_none() {
        claim.error = Some(released_error());
    }
    record.env.clear();
    record.endpoint_token = "0".repeat(64);
    record.sandbox.image.uri.clear();
    record.sandbox.image.auth = None;
    record.sandbox.entrypoint.clear();
    record.sandbox.metadata.clear();
    record.sandbox.expires_at = None;
    record.sandbox.status = SandboxStatus {
        state: "Failed".into(),
        message: Some("claim released".into()),
        last_transition_at: Some(Utc::now()),
    };
}

#[cfg(any(test, feature = "cluster"))]
fn view(record: &Record) -> ClaimView {
    let claim = record.claim.as_ref().expect("claim view");
    ClaimView {
        identity: claim.identity.clone(),
        sandbox: record.sandbox.clone(),
        phase: claim.phase,
        error: claim.error.clone(),
        creation_result: claim.creation_result.clone(),
    }
}

// Internal durable queue payload; never accepted by the public OpenSandbox parser.
#[cfg(any(test, feature = "cluster"))]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub(super) enum QueuedCreate {
    E2b {
        e2b_request: super::e2b::Create,
        profile: crate::runtime::E2bTemplate,
    },
    Open(CreateRequest),
}
#[cfg(any(test, feature = "cluster"))]
impl QueuedCreate {
    pub(super) fn validate(self, maximum: u64) -> Result<super::models::Validated, ApiError> {
        match self {
            Self::Open(request) => request.validate(maximum),
            Self::E2b {
                e2b_request,
                profile,
            } => e2b_request.translate(profile, maximum).map_err(|e| e.0),
        }
    }
}

#[cfg(any(test, feature = "cluster"))]
impl Daemon {
    /// Atomically reserve against ordinary/E2B creates, durably before accepting OFFER.
    /// Exact request retries are idempotent; mismatches conflict, even after release.
    #[cfg(test)]
    pub(super) async fn reserve_claim(
        self: &Arc<Self>,
        identity: ClaimIdentity,
        request: CreateRequest,
    ) -> Result<Sandbox, ApiError> {
        self.reserve_queued_claim(identity, QueuedCreate::Open(request))
            .await
    }

    pub(super) async fn reserve_queued_claim(
        self: &Arc<Self>,
        identity: ClaimIdentity,
        request: QueuedCreate,
    ) -> Result<Sandbox, ApiError> {
        let this = self.clone();
        tokio::spawn(async move {
            validate_identity(&identity)?;
            let bytes =
                serde_json::to_vec(&request).map_err(|e| ApiError::internal(e.to_string()))?;
            let request_digest = Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let validated = request.validate(this.config.max_timeout_seconds)?;
            if let Some(expected) = &validated.e2b {
                let actual = this
                    .runtime
                    .e2b_template(&validated.image.uri)
                    .await
                    .map_err(|_| {
                        ApiError::new(
                            404,
                            "TEMPLATE_NOT_FOUND",
                            "Prepared E2B template is unavailable",
                        )
                    })?;
                if &actual != expected {
                    return Err(ApiError::new(
                        409,
                        "TEMPLATE_MISMATCH",
                        "Executor template profile differs from Home",
                    ));
                }
            }
            let claim = ClaimRecord {
                identity,
                phase: ClaimPhase::Reserved,
                request_digest,
                timeout: validated.timeout,
                error: None,
                creation_result: None,
                storage_charge: 0,
            };
            this.create_admitted(validated, Some(claim)).await
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    }

    async fn claim_record(&self, identity: &ClaimIdentity) -> Result<Record, ApiError> {
        validate_identity(identity)?;
        let record = self.record(&format!("sb-{}", identity.claim_id)).await?;
        if record
            .claim
            .as_ref()
            .is_none_or(|claim| claim.identity != *identity)
        {
            return Err(conflict());
        }
        Ok(record)
    }

    /// Scheduler-only: caller must first obtain durable Home Assigned authorization.
    /// Detached from cancellation; the durable Launching fence precedes runtime.create.
    /// Neither retry nor restart can reissue create after that fence, even in its crash gap.
    pub(super) async fn launch_claim(
        self: &Arc<Self>,
        identity: &ClaimIdentity,
    ) -> Result<Sandbox, ApiError> {
        let this = self.clone();
        let identity = identity.clone();
        tokio::spawn(async move {
            let id = format!("sb-{}", identity.claim_id);
            let gate = this.operation(&id).await?;
            let _operation = gate.write().await;
            let mut record = this.claim_record(&identity).await?;
            let claim = record.claim.as_mut().unwrap();
            match claim.phase {
                ClaimPhase::Released => {
                    return Err(claim.error.clone().unwrap_or_else(released_error));
                }
                ClaimPhase::Launching => {
                    return Err(claim.error.clone().unwrap_or_else(|| {
                        ApiError::new(
                            503,
                            "CLAIM_LAUNCH_UNCERTAIN",
                            "launch already attempted; inspect/delete, never recreate",
                        )
                    }));
                }
                ClaimPhase::Finished => {
                    return claim.creation_result.clone().unwrap_or_else(|| {
                        Err(ApiError::new(
                            503,
                            "CLAIM_LAUNCH_UNCERTAIN",
                            "terminal create outcome not established",
                        ))
                    });
                }
                ClaimPhase::Reserved => {}
            }
            claim.phase = ClaimPhase::Launching;
            claim.error = Some(ApiError::new(
                503,
                "CLAIM_LAUNCH_UNCERTAIN",
                "launch intent committed; runtime outcome not yet confirmed",
            ));
            record.sandbox.status.state = "Pending".into();
            record.sandbox.status.last_transition_at = Some(Utc::now());
            record.sandbox.expires_at = claim
                .timeout
                .map(|seconds| Utc::now() + Duration::seconds(seconds as i64));
            let spec = RuntimeSpec {
                id: id.clone(),
                image: record.sandbox.image.uri.clone(),
                entrypoint: record.sandbox.entrypoint.clone(),
                env: record.env.clone(),
                cpu_millis: record.cpu_millis,
                memory_bytes: record.memory_bytes,
                e2b: record.e2b.clone(),
            };
            this.update(&id, |_| Ok((Some(record.clone()), ()))).await?;
            match this.runtime.create(&spec).await {
                Ok(()) => {
                    record.claim.as_mut().unwrap().phase = ClaimPhase::Finished;
                    record.claim.as_mut().unwrap().error = None;
                    record.sandbox.status.state = "Running".into();
                }
                Err(error) => {
                    let failure = ApiError::new(
                        500,
                        "SANDBOX_CREATE_FAILED",
                        format!(
                            "sandbox {id} launch failed; inspect/delete this ID, never resubmit"
                        ),
                    );
                    record.claim.as_mut().unwrap().error = Some(failure.clone());
                    record.sandbox.status.state = "Failed".into();
                    if error
                        .downcast_ref::<crate::runtime::CreateError>()
                        .is_some_and(|e| !e.requires_reconciliation())
                    {
                        record.claim.as_mut().unwrap().creation_result = Some(Err(failure.clone()));
                        tombstone(&mut record);
                    }
                    this.update(&id, |_| Ok((Some(record), ()))).await?;
                    return Err(failure);
                }
            }
            record.sandbox.status.last_transition_at = Some(Utc::now());
            let sandbox = record.sandbox.clone();
            record.claim.as_mut().unwrap().creation_result = Some(Ok(sandbox.clone()));
            this.update(&id, |_| Ok((Some(record), sandbox))).await
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    }

    /// Only for explicit Home rejection before launch. Never cleans an uncertain runtime.
    pub(super) async fn release_claim(
        self: &Arc<Self>,
        identity: &ClaimIdentity,
    ) -> Result<(), ApiError> {
        let this = self.clone();
        let identity = identity.clone();
        tokio::spawn(async move {
            let id = format!("sb-{}", identity.claim_id);
            let gate = this.operation(&id).await?;
            let _operation = gate.write().await;
            let mut record = this.claim_record(&identity).await?;
            match record.claim.as_ref().unwrap().phase {
                ClaimPhase::Released => return Ok(()),
                ClaimPhase::Reserved => tombstone(&mut record),
                _ => {
                    return Err(ApiError::new(
                        409,
                        "CLAIM_ALREADY_LAUNCHED",
                        "launch fence committed; explicit confirmed sandbox deletion required",
                    ));
                }
            }
            this.update(&id, |_| Ok((Some(record), ()))).await
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    }

    /// Reads last durable observation without runtime side effects; checks full identity.
    pub(super) async fn inspect_claim(
        &self,
        identity: &ClaimIdentity,
    ) -> Result<ClaimView, ApiError> {
        Ok(view(&self.claim_record(identity).await?))
    }

    /// Recovery inventory of non-released claims (including Finished observations).
    /// A fail-stopped store yields no work; every subsequent mutation also fails closed.
    pub(super) async fn pending_claims(&self) -> Vec<ClaimView> {
        if self.ensure_storage().is_err() {
            return Vec::new();
        }
        self.registry
            .lock()
            .await
            .sandboxes
            .values()
            .filter(|r| r.claim.is_some() && r.holds_capacity())
            .map(view)
            .collect()
    }

    /// Count, CPU millis and memory bytes held, including Reserved/uncertain claims.
    /// Only confirmed Released tombstones consume zero capacity.
    pub(super) async fn capacity_usage(&self) -> Result<(u64, u64, u64), ApiError> {
        self.ensure_storage()?;
        self.registry
            .lock()
            .await
            .sandboxes
            .values()
            .filter(|r| r.holds_capacity())
            .try_fold((0u64, 0u64, 0u64), |(count, cpu, memory), r| {
                Ok((
                    count
                        .checked_add(1)
                        .ok_or_else(|| ApiError::internal("capacity overflow"))?,
                    cpu.checked_add(r.cpu_millis)
                        .ok_or_else(|| ApiError::internal("capacity overflow"))?,
                    memory
                        .checked_add(r.memory_bytes)
                        .ok_or_else(|| ApiError::internal("capacity overflow"))?,
                ))
            })
    }
}
