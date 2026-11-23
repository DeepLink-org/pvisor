use super::storage::{Storage, StoredObject, conflict};
use anyhow::{Context, ensure};
use axum::http::{Request, Response, StatusCode, header};
use object_store::{PutMode, UpdateVersion};
use opendal::{
    Buffer, ErrorKind, HttpBody, HttpTransport, HttpTransporter, Operator, services::Gcs,
};
use std::{ops::Range, sync::Arc, time::Duration};
use tokio_stream::StreamExt;

pub(crate) const ENDPOINT_ENV: &str = "PVISOR_GCS_ENDPOINT";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);

pub(super) fn gcs(bucket: &str, prefix: &str, read_only: bool) -> anyhow::Result<Storage> {
    let mut builder = Gcs::default().bucket(bucket).root(prefix);
    if let Some(endpoint) = crate::env::unicode(ENDPOINT_ENV)? {
        let url = super::http::Http::validate(&endpoint)?;
        ensure!(
            url.scheme() == "https"
                || url
                    .host_str()
                    .is_some_and(|host| matches!(host, "127.0.0.1" | "[::1]" | "localhost")),
            "GCS endpoint requires HTTPS outside loopback"
        );
        builder = builder.endpoint(&endpoint);
    }
    let operator = operator(builder)?;
    Ok(Storage::OpenDal(Arc::new(Remote::new(
        operator, read_only,
    )?)))
}

pub(crate) struct Remote {
    operator: Operator,
    runtime: Arc<super::s3_runtime::Runtime>,
    read_only: bool,
}

fn operator(builder: Gcs) -> anyhow::Result<Operator> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .build()?;
    let operator = Operator::new(builder)?;
    let context = operator
        .context()
        .with_http_transport(HttpTransporter::new(Transport(client)));
    Ok(operator.with_context(context))
}

struct Transport(reqwest::Client);

impl HttpTransport for Transport {
    async fn fetch(&self, request: Request<Buffer>) -> opendal::Result<Response<HttpBody>> {
        let (parts, body) = request.into_parts();
        let range = parts.headers.get(header::RANGE).cloned();
        let response = self
            .0
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .header(header::ACCEPT_ENCODING, "identity")
            .body(body.to_bytes())
            .send()
            .await
            .map_err(http_error)?;
        let status = response.status();
        if status.is_success()
            && let Some(range) = range
        {
            let expected = range
                .to_str()
                .ok()
                .and_then(|value| value.strip_prefix("bytes="));
            let actual = response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("bytes "))
                .and_then(|value| value.split_once('/').map(|(range, _)| range));
            if status != StatusCode::PARTIAL_CONTENT || expected.is_none() || actual != expected {
                return Err(opendal::Error::new(
                    ErrorKind::Unexpected,
                    "invalid OpenDAL HTTP range",
                ));
            }
        }
        let mut reply = Response::builder().status(status);
        *reply.headers_mut().expect("valid HTTP status") = response.headers().clone();
        let media = parts
            .uri
            .query()
            .is_some_and(|query| query.split('&').any(|part| part == "alt=media"));
        let limit = if !status.is_success() || !media {
            1024 * 1024
        } else {
            super::storage::MAX_OBJECT
        };
        let mut received = 0usize;
        let stream = response.bytes_stream().map(move |result| {
            let bytes = result.map_err(http_error)?;
            received = received.saturating_add(bytes.len());
            if received > limit {
                return Err(opendal::Error::new(
                    ErrorKind::Unexpected,
                    "oversized OpenDAL HTTP response",
                ));
            }
            Ok(Buffer::from(bytes))
        });
        Ok(reply
            .body(HttpBody::new(stream, None))
            .expect("valid HTTP response"))
    }
}

fn http_error(error: reqwest::Error) -> opendal::Error {
    opendal::Error::new(ErrorKind::Unexpected, "OpenDAL HTTP transport failed")
        .set_source(error.without_url())
}

impl Remote {
    fn new(operator: Operator, read_only: bool) -> anyhow::Result<Self> {
        let capability = operator.info().capability();
        ensure!(
            capability.read && capability.stat,
            "OpenDAL backend lacks object reads"
        );
        ensure!(
            read_only
                || (capability.write
                    && capability.write_with_if_not_exists
                    && capability.write_with_if_version_match
                    && capability.read_with_if_version_match),
            "OpenDAL backend lacks atomic generation conditions"
        );
        Ok(Self {
            operator,
            runtime: super::s3_runtime::Runtime::shared()?,
            read_only,
        })
    }

    pub(super) fn read(
        &self,
        key: &str,
        range: Option<Range<u64>>,
        limit: usize,
        timeout: Option<Duration>,
    ) -> anyhow::Result<Option<StoredObject>> {
        let operator = self.operator.clone();
        let key = key.to_owned();
        self.runtime
            .run(timeout.unwrap_or(REQUEST_TIMEOUT), async move {
                let result = async {
                    let mut reader = operator.reader_with(&key);
                    let (range, version) = if let Some(range) = range {
                        (
                            range,
                            UpdateVersion {
                                e_tag: None,
                                version: None,
                            },
                        )
                    } else {
                        let meta = operator.stat(&key).await?;
                        ensure!(
                            meta.is_file() && meta.content_length() <= limit as u64,
                            "cache object exceeds size limit or is not a file"
                        );
                        if let Some(generation) = meta.version() {
                            reader = reader.if_version_match(generation);
                        } else {
                            anyhow::bail!("OpenDAL object is missing a generation");
                        }
                        (
                            0..meta.content_length(),
                            UpdateVersion {
                                e_tag: meta.etag().map(str::to_owned),
                                version: meta.version().map(str::to_owned),
                            },
                        )
                    };
                    let length = range
                        .end
                        .checked_sub(range.start)
                        .context("invalid object range")?;
                    ensure!(length <= limit as u64, "cache range exceeds size limit");
                    let mut bytes = Vec::new();
                    if length != 0 {
                        let mut body = reader.await?.into_stream(range).await?;
                        while let Some(chunk) = body.next().await {
                            let chunk = chunk?;
                            ensure!(
                                chunk.len() as u64 <= length - bytes.len() as u64,
                                "oversized OpenDAL cache body"
                            );
                            for segment in chunk {
                                bytes.extend_from_slice(&segment);
                            }
                        }
                        ensure!(bytes.len() as u64 == length, "truncated OpenDAL cache body");
                    }
                    Ok::<_, anyhow::Error>(StoredObject { bytes, version })
                }
                .await;
                match result {
                    Err(error)
                        if error
                            .downcast_ref::<opendal::Error>()
                            .is_some_and(|e| e.kind() == ErrorKind::NotFound) =>
                    {
                        Ok(None)
                    }
                    result => result.map(Some),
                }
            })
    }

    pub(super) fn write(&self, key: &str, bytes: Vec<u8>, mode: PutMode) -> anyhow::Result<()> {
        ensure!(!self.read_only, "OpenDAL repository is read-only");
        let operator = self.operator.clone();
        let key = key.to_owned();
        self.runtime.run(REQUEST_TIMEOUT, async move {
            let write = operator.write_with(&key, bytes);
            let write = match &mode {
                PutMode::Create => write.if_not_exists(true),
                PutMode::Update(version) => write.if_version_match(
                    version
                        .version
                        .as_deref()
                        .context("missing OpenDAL generation for conditional write")?,
                ),
                PutMode::Overwrite => write,
            };
            match write.await {
                Ok(_) => Ok(None),
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::AlreadyExists | ErrorKind::ConditionNotMatch
                    ) =>
                {
                    Err(conflict())
                }
                Err(error) => Err(error.into()),
            }
        })?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "opendal_store_tests.rs"]
mod tests;
