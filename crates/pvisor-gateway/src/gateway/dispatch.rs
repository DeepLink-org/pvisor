//! Gateway sink adapter for the OverlayNet proxy server.

use async_trait::async_trait;
use axum::Router;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use pvisor_overlaynet::policy::{DenyReason, NetworkPolicy};
use pvisor_overlaynet::server::{OverlayRequestContext, OverlayServerState, OverlaySink};

use super::llm_capture::llm_capture;
use super::state::GatewayState;
use crate::config::ProxyConfig;
use crate::runtime::debug::{self, is_debug_enabled};
use crate::session::storage::resolve_capture_route_with_root;

#[derive(Clone)]
pub(crate) struct GatewayRequestContext {
    config: std::sync::Arc<ProxyConfig>,
    debug_on: bool,
}

pub(crate) fn build_router(state: GatewayState) -> Router {
    let client_token = state.client_token.clone();
    let server = OverlayServerState::new(
        state.control_controller.clone(),
        state.clone(),
        state.active_requests.clone(),
    )
    .with_interception_metrics(state.interception_metrics.clone())
    .with_bandwidth_registry(state.bandwidth_registry.clone());
    let router = pvisor_overlaynet::server::build_router(server);
    let Some(token) = client_token else {
        return router;
    };
    router.layer(axum::middleware::from_fn(
        move |mut request: Request, next: axum::middleware::Next| {
            let token = token.clone();
            async move {
                let model_request = request.method() != axum::http::Method::CONNECT
                    && (request.uri().authority().is_none()
                        || crate::protocol::ProtocolKind::from_path(request.uri().path())
                            != crate::protocol::ProtocolKind::Unknown);
                if model_request && !super::auth::consume_local_token(request.headers_mut(), &token)
                {
                    return (
                        StatusCode::UNAUTHORIZED,
                        "Attempt Gateway credential required",
                    )
                        .into_response();
                }
                next.run(request).await
            }
        },
    ))
}

#[async_trait]
impl OverlaySink for GatewayState {
    type RequestContext = GatewayRequestContext;

    fn request_context(
        &self,
        request: &Request,
    ) -> anyhow::Result<OverlayRequestContext<Self::RequestContext>> {
        let route = resolve_capture_route_with_root(
            request.headers(),
            &Bytes::new(),
            &self.config.session_header,
            self.root_session.clone(),
        );
        // Capture files and client-selected sessions never grant policy authority.
        let config = std::sync::Arc::clone(&self.config);
        let debug_on = is_debug_enabled(&config, self.storage.as_path());
        Ok(OverlayRequestContext {
            policy: NetworkPolicy::from_config(config.as_ref())?,
            run_id: route.root_session,
            attempt_id: self.attempt_id.clone(),
            session_id: route.session_id,
            sink: GatewayRequestContext { config, debug_on },
        })
    }

    async fn handle(
        &self,
        request: Request,
        peer: std::net::SocketAddr,
        context: &OverlayRequestContext<Self::RequestContext>,
    ) -> anyhow::Result<Response> {
        if !self.gateway_enabled {
            return Ok((
                StatusCode::BAD_REQUEST,
                "relative request requires Gateway mode",
            )
                .into_response());
        }
        llm_capture(
            self.clone(),
            request,
            peer,
            std::sync::Arc::clone(&context.sink.config),
            context.sink.debug_on,
        )
        .await
    }

    fn download_wait(
        &self,
        _context: &OverlayRequestContext<Self::RequestContext>,
    ) -> anyhow::Result<Option<Box<dyn pvisor_overlaynet::server::DownloadWait>>> {
        match &self.model_wait {
            Some(lifecycle) => lifecycle.reserve_download(),
            None => Ok(None),
        }
    }

    fn accepts(&self, request: &Request) -> bool {
        self.gateway_enabled
            && crate::protocol::ProtocolKind::from_path(request.uri().path())
                != crate::protocol::ProtocolKind::Unknown
    }

    fn on_denied(
        &self,
        context: &OverlayRequestContext<Self::RequestContext>,
        host: &str,
        reason: &DenyReason,
    ) {
        if context.sink.debug_on {
            debug::log_network_denied(
                self.storage.as_path(),
                host,
                context.policy.mode_str(),
                reason.as_str(),
                &context.session_id,
            );
        }
    }

    fn on_dispatch(
        &self,
        context: &OverlayRequestContext<Self::RequestContext>,
        request: &Request,
        target: &str,
    ) {
        if context.sink.debug_on {
            debug::log_dispatch(
                self.storage.as_path(),
                request.method().as_str(),
                &request.uri().to_string(),
                &context.session_id,
                target,
            );
        }
    }

    fn on_proxy_result(
        &self,
        context: &OverlayRequestContext<Self::RequestContext>,
        target: &str,
        status: StatusCode,
    ) {
        if context.sink.debug_on {
            debug::log_proxy_result(
                self.storage.as_path(),
                target,
                status.as_u16(),
                &context.session_id,
            );
        }
    }

    fn on_proxy_error(
        &self,
        context: &OverlayRequestContext<Self::RequestContext>,
        target: &str,
        error: &str,
    ) {
        if context.sink.debug_on {
            debug::log_proxy_error(self.storage.as_path(), target, error, &context.session_id);
        }
    }
}
