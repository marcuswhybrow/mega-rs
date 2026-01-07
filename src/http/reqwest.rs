use std::io;
use std::pin::Pin;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use futures::io::AsyncRead;
use futures::TryStreamExt;
use json::Value;
use reqwest::{Body, StatusCode};
use secrecy::ExposeSecret;
use tokio_util::codec::{BytesCodec, FramedRead};
use tokio_util::compat::FuturesAsyncReadCompatExt;
use url::Url;

use crate::error::{Error, Result};
use crate::http::HttpClient;
use crate::protocol::commands::{Request, Response};
use crate::utils::hashcash::{gencash, parse_hashcash_header};
use crate::{ClientState, ErrorCode};

#[async_trait]
impl HttpClient for reqwest::Client {
    #[tracing::instrument(skip(self, state, query_params))]
    async fn send_requests(
        &self,
        state: &ClientState,
        requests: &[Request],
        query_params: &[(&str, &str)],
    ) -> Result<Vec<Response>> {
        tracing::trace!(?self, ?state, "preparing MEGA request");

        let url = {
            let mut url = state.origin.join("/cs")?;

            let mut qs = url.query_pairs_mut();
            let id_counter = state.id_counter.fetch_add(1, Ordering::SeqCst);
            qs.append_pair("id", id_counter.to_string().as_str());

            if let Some(session) = state.session.as_ref() {
                qs.append_pair("sid", session.expose_secret().session_id.as_str());
            }

            qs.extend_pairs(query_params);

            qs.finish();
            drop(qs);

            url
        };

        let mut delay = state.min_retry_delay;
        let mut hashcash_challenge: Option<(String, u8)> = None;

        for attempt in 1..=state.max_retries {
            if attempt > 1 && hashcash_challenge.is_none() {
                tracing::debug!(?delay, "sleeping for exponential back‑off before retrying");
                tokio::time::sleep(delay).await;
                delay = std::cmp::min(delay * 2, state.max_retry_delay);
            }

            let mut builder = self.post(url.clone());

            if let Some((ref token, easiness)) = hashcash_challenge {
                // Use a blocking worker to generate the hashcash stamp. This allows the CPU to
                // be used more efficiently, instead of blocking the tokio runtime.
                let stamp = tokio::task::spawn_blocking({
                    let token = token.clone();
                    move || gencash(&token, easiness)
                })
                .await
                .expect("hashcash worker panicked");
                let header_value = format!("1:{token}:{stamp}");
                builder = builder.header("x-hashcash", header_value.clone());
                tracing::trace!(header=%header_value, "attached solved X‑Hashcash header");

                hashcash_challenge = None;
            }

            let body = json::to_string(&requests).unwrap();
            tracing::info!(json = %body, "Sending request");

            let request_fut = builder.json(requests).send();

            let response = match if let Some(timeout) = state.timeout {
                tokio::time::timeout(timeout, request_fut).await
            } else {
                Ok(request_fut.await)
            } {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    tracing::error!(?e, "network error while making MEGA request");
                    continue;
                }
                Err(e) => {
                    tracing::error!(?e, "timeout while making MEGA request");
                    continue;
                }
            };

            let status = response.status().to_string();

            // ─────────────────────────────────────────────────────────────────────
            // 409 = Payment‑Required → the server is challenging us with Hashcash
            // ─────────────────────────────────────────────────────────────────────
            if response.status() == StatusCode::PAYMENT_REQUIRED {
                tracing::debug!("received 409 – server requests Hashcash proof‑of‑work");

                if let Some((token, easiness)) = response
                    .headers()
                    .get("x-hashcash")
                    .and_then(parse_hashcash_header)
                {
                    hashcash_challenge = Some((token.clone(), easiness));
                    tracing::trace!(token = %token, easiness, "parsed Hashcash challenge");
                    continue;
                }

                tracing::error!("409 received but no valid Hashcash challenge found — aborting");
                return Err(Error::MaxRetriesReached);
            }

            // ─────────────────────────────────────────────────────────────────────
            // The response did not ask for Hashcash – handle as usual
            // ─────────────────────────────────────────────────────────────────────
            let response_bytes = match response.error_for_status() {
                Ok(ok) => {
                    let bytes = ok.bytes().await?;
                    let body = String::from_utf8_lossy(&bytes);
                    tracing::info!(status = %status, body = %body, "Response");
                    bytes
                },
                Err(err) => {
                    tracing::error!(%err, "HTTP error status, will retry");
                    continue;
                }
            };

            if let Ok(code) = json::from_slice::<ErrorCode>(&response_bytes) {
                if code == ErrorCode::EAGAIN {
                    tracing::debug!(?code, "MEGA returned error code EAGAIN (request failed but may be retried)");
                    continue;
                }
                if code != ErrorCode::OK {
                    tracing::error!(?code, "MEGA error code");
                }
                return Err(Error::from(code));
            }

            let responses: Vec<Value> = json::from_slice(&response_bytes).map_err(|e| {
                tracing::error!(?e, "could not deserialize MEGA response array");
                e
            })?;

            return requests
                .iter()
                .zip(responses)
                .map(|(req, resp)| req.parse_response_data(resp))
                .collect();
        }

        tracing::error!("maximum retries reached, cancelling MEGA request");
        Err(Error::MaxRetriesReached)
    }

    async fn get(&self, url: Url) -> Result<Pin<Box<dyn AsyncRead + Send>>> {
        let stream = self
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes_stream()
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err));

        Ok(Box::pin(stream.into_async_read()))
    }

    async fn post(
        &self,
        url: Url,
        body: Pin<Box<dyn AsyncRead + Send + Sync>>,
        content_length: Option<u64>,
    ) -> Result<Pin<Box<dyn AsyncRead + Send>>> {
        let stream = FramedRead::new(body.compat(), BytesCodec::new());
        let body = Body::wrap_stream(stream);
        let stream = {
            let mut builder = self.post(url);

            if let Some(content_length) = content_length {
                builder = builder.header("content-length", content_length);
            }

            builder
                .body(body)
                .send()
                .await?
                .error_for_status()?
                .bytes_stream()
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err))
        };

        Ok(Box::pin(stream.into_async_read()))
    }
}
