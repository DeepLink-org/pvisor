use crate::*;
use serde::{Serialize, de::DeserializeOwned};
use std::time::Duration;

#[derive(Clone)]
pub struct Client { http: reqwest::Client, base: String, token: String }
impl Client {
    pub fn new(base: &str, token: String) -> anyhow::Result<Self> {
        let url = reqwest::Url::parse(base)?;
        anyhow::ensure!(matches!(url.scheme(), "http" | "https") && url.host_str().is_some(), "controller URL must be HTTP(S)");
        Ok(Self { http: reqwest::Client::builder().timeout(Duration::from_secs(5)).redirect(reqwest::redirect::Policy::none()).build()?,
            base: base.trim_end_matches('/').to_owned(), token })
    }
    async fn post<T: Serialize + ?Sized, R: DeserializeOwned>(&self, path: &str, body: &T) -> anyhow::Result<R> {
        Ok(self.http.post(format!("{}{path}", self.base)).bearer_auth(&self.token).json(body).send().await?.error_for_status()?.json().await?)
    }
    async fn get<R: DeserializeOwned>(&self, path: &str) -> anyhow::Result<R> {
        Ok(self.http.get(format!("{}{path}", self.base)).bearer_auth(&self.token).send().await?.error_for_status()?.json().await?)
    }
    pub async fn submit(&self, spec: &TaskSpec) -> anyhow::Result<TaskRecord> { self.post("/v1/tasks", spec).await }
    pub async fn task(&self, id: &str) -> anyhow::Result<TaskRecord> { self.get(&format!("/v1/tasks/{id}")).await }
    pub async fn cancel(&self, id: &str) -> anyhow::Result<TaskRecord> { self.post(&format!("/v1/tasks/{id}/cancel"), &()).await }
    pub async fn workers(&self) -> anyhow::Result<Vec<WorkerRecord>> { self.get("/v1/workers").await }
    pub async fn register(&self, spec: &WorkerRegistration) -> anyhow::Result<WorkerRecord> { self.post("/v1/workers/register", spec).await }
    pub async fn poll(&self, request: &PollRequest) -> anyhow::Result<PollResponse> { self.post("/v1/workers/poll", request).await }
    pub async fn complete(&self, completion: &Completion) -> anyhow::Result<TaskRecord> { self.post("/v1/workers/complete", completion).await }
    pub async fn drain(&self, id: &str, draining: bool) -> anyhow::Result<serde_json::Value> { self.post(&format!("/v1/workers/{id}/drain"), &serde_json::json!({"draining":draining})).await }
}
