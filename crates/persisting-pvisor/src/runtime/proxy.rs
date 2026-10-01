//! Plain explicit proxy; policy and forwarding remain owned by OverlayNet.
use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use persisting_control::ControlController;
use persisting_overlaynet::server::{OverlayRequestContext, OverlayServerState, OverlaySink};
use persisting_overlaynet::{BandwidthRegistry, InterceptionMetrics, NetworkPolicy};
use std::{
    net::SocketAddr,
    sync::{Arc, atomic::AtomicUsize},
    thread::JoinHandle,
};
use tokio::sync::oneshot;

#[derive(Clone)]
struct Sink {
    policy: NetworkPolicy,
    run_id: String,
    attempt_id: String,
}
#[async_trait::async_trait]
impl OverlaySink for Sink {
    type RequestContext = ();
    fn request_context(&self, _request: &Request) -> anyhow::Result<OverlayRequestContext<()>> {
        Ok(OverlayRequestContext {
            policy: self.policy.clone(),
            run_id: Some(self.run_id.clone()),
            attempt_id: Some(self.attempt_id.clone()),
            storyline_id: None,
            session_id: self.run_id.clone(),
            sink: (),
        })
    }
    async fn handle(
        &self,
        _request: Request,
        _peer: SocketAddr,
        _context: &OverlayRequestContext<()>,
    ) -> anyhow::Result<Response> {
        Ok((StatusCode::NOT_FOUND, "Gateway capture is disabled").into_response())
    }
}

pub(crate) struct Proxy {
    pub listen: String,
    stop: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<anyhow::Result<()>>>,
}
impl Proxy {
    pub fn start(
        listen: &str,
        policy: NetworkPolicy,
        controller: Arc<dyn ControlController>,
        metrics: InterceptionMetrics,
        run_id: String,
        attempt_id: String,
    ) -> anyhow::Result<Self> {
        let listener = std::net::TcpListener::bind(listen)?;
        listener.set_nonblocking(true)?;
        let listen = listener.local_addr()?.to_string();
        let (stop, stopped) = oneshot::channel();
        let state = OverlayServerState::new(
            controller,
            Sink {
                policy,
                run_id,
                attempt_id,
            },
            Arc::new(AtomicUsize::new(0)),
        )
        .with_interception_metrics(metrics)
        .with_bandwidth_registry(BandwidthRegistry::default());
        let join = std::thread::Builder::new()
            .name("pvisor-proxy".into())
            .spawn(move || {
                tokio::runtime::Runtime::new()?.block_on(async {
                    use std::future::IntoFuture;
                    let server = axum::serve(
                        tokio::net::TcpListener::from_std(listener)?,
                        persisting_overlaynet::server::build_router(state)
                            .into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .into_future();
                    tokio::select! {
                        result = server => result?,
                        _ = stopped => {},
                    }
                    Ok(())
                })
            })?;
        Ok(Self {
            listen,
            stop: Some(stop),
            join: Some(join),
        })
    }
    pub fn shutdown(mut self) -> anyhow::Result<()> {
        self.shutdown_inner()
    }
    fn shutdown_inner(&mut self) -> anyhow::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(join) = self.join.take() {
            join.join()
                .map_err(|_| anyhow::anyhow!("proxy thread panicked"))??;
        }
        Ok(())
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.shutdown_inner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_proxy_enforces_policy_and_releases_its_listener() {
        let metrics = InterceptionMetrics::default();
        let policy = NetworkPolicy::compile(&persisting_overlaynet::NetworkConfig {
            mode: persisting_overlaynet::NetworkMode::NoNetwork,
            ..Default::default()
        })
        .unwrap();
        let proxy = Proxy::start(
            "127.0.0.1:0",
            policy.clone(),
            Arc::new(persisting_control::PolicyControlController),
            metrics,
            "run".into(),
            "attempt".into(),
        )
        .unwrap();
        let address = proxy.listen.clone();
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .proxy(reqwest::Proxy::http(format!("http://{address}")).unwrap())
            .build()
            .unwrap();
        let response = client.get("http://example.com/").send().unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        // An idle client must not keep Attempt teardown waiting for its connection.
        proxy.shutdown().unwrap();
        assert!(std::net::TcpStream::connect(&address).is_err());
        assert!(
            Proxy::start(
                "invalid-listen",
                policy,
                Arc::new(persisting_control::PolicyControlController),
                InterceptionMetrics::default(),
                "run".into(),
                "attempt".into()
            )
            .is_err()
        );
    }
}
