//! Worker server implementation using Axum

use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU16, AtomicU32, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    serve::Serve,
};
use color_eyre::Result;
use rand::Rng;
use serde::{Deserialize, Serialize, ser::SerializeMap};
use thiserror::Error;
use tokio::net::TcpListener;
use tower::{Layer, Service};
use tower_http::trace::TraceLayer;

use crate::tracing::{make_span_with_request_id, on_request, on_response};

pub struct WorkerServer {
    pub config: WorkerConfig,
    server: Serve<TcpListener, Router, Router>,
}

pub type WorkerId = u32;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerConfig {
    pub id: WorkerId,
    pub address: String,
}

/// State that contains information that can be useful for the load balancer
#[derive(Clone, Debug)]
pub struct WorkerState {
    config: WorkerConfig,
    nb_requests_received: Arc<AtomicU32>,
    nb_requests_being_handled: Arc<AtomicU16>,
}

impl WorkerServer {
    pub async fn build(id: u32, address: &str) -> Result<Self> {
        let listener = TcpListener::bind(address).await?;
        let address = listener.local_addr()?.to_string();
        let config = WorkerConfig { id, address };
        let state = WorkerState {
            config: config.clone(),
            nb_requests_received: Arc::new(AtomicU32::new(0)),
            nb_requests_being_handled: Arc::new(AtomicU16::new(0)),
        };
        let router = Router::new()
            .route("/work", axum::routing::post(work))
            .route("/check-health", get(check_health))
            .route("/state", get(get_worker_state))
            .layer(RequestStatsLayer {
                state: state.clone(),
            })
            .with_state(state)
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(make_span_with_request_id)
                    .on_request(on_request)
                    .on_response(on_response),
            );
        let server = axum::serve(listener, router);
        Ok(WorkerServer { config, server })
    }
    pub async fn run(self) -> Result<(), std::io::Error> {
        tracing::info!("Starting worker on {}", self.config.address);
        self.server.await
    }

    pub async fn create_and_run_worker() -> WorkerConfig {
        const LOCAL_ADDR: &'static str = "127.0.0.1:0";
        let id = rand::rng().random::<u32>();
        let worker = WorkerServer::build(id, LOCAL_ADDR)
            .await
            .expect("Failed to run server");
        let config = worker.config.clone();
        tokio::spawn(worker.run());
        config
    }
}

/// Check health of the worker
#[tracing::instrument(name = "Health Check")]
pub async fn check_health(
    State(state): State<WorkerState>,
) -> std::result::Result<impl IntoResponse, WorkerError> {
    Ok(StatusCode::OK)
}

/// Send work to worker
#[tracing::instrument(name = "Work")]
pub async fn work(
    State(_state): State<WorkerState>,
    Json(request): Json<WorkRequest>,
) -> std::result::Result<impl IntoResponse, WorkerError> {
    // Delay the response to simulate some work ongoing
    tokio::time::sleep(Duration::from_millis(request.duration)).await;
    Ok(StatusCode::OK)
}

#[derive(Serialize, Deserialize, Debug)]
struct WorkRequest {
    duration: u64,
}

pub async fn get_worker_state(
    State(state): State<WorkerState>,
) -> std::result::Result<impl IntoResponse, WorkerError> {
    let json = serde_json::to_string_pretty(&state).map_err(|e| WorkerError(e.into()))?;
    Ok(Response::new(Body::from(json)))
}

#[derive(Debug, Error)]
pub struct WorkerError(color_eyre::Report);
impl IntoResponse for WorkerError {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Implement a middleware that gives us basic statistics on request
/// received by the worker server
#[derive(Clone)]
struct RequestStatsLayer {
    state: WorkerState,
}

#[derive(Clone)]
struct RequestStatsService<S> {
    inner: S,
    state: WorkerState,
}

impl<S> Layer<S> for RequestStatsLayer {
    type Service = RequestStatsService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RequestStatsService {
            inner,
            state: self.state.clone(),
        }
    }
}

// Inspired from https://github.com/tokio-rs/axum/discussions/236
// Our service get stats on the requests that have been handled by the worker
// - number of requests handled in total
// - number of requests being handled currently
impl<S, RequestBody, ResponseBody> Service<Request<RequestBody>> for RequestStatsService<S>
where
    S: Service<Request<RequestBody>, Response = Response<ResponseBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    RequestBody: Send + 'static,
    ResponseBody: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<RequestBody>) -> Self::Future {
        // Increment the number of connections received
        self.state
            .nb_requests_received
            .fetch_add(1, Ordering::Relaxed);
        // Increment the number of connections being handled
        self.state
            .nb_requests_being_handled
            .fetch_add(1, Ordering::Relaxed);

        let state_clone = self.state.clone();

        // Create a response in a future
        let fut = self.inner.call(req);

        // Wrap the future in another one to be able to decrement nb_of_requests_being_handled
        // when the the request has been processed
        Box::pin(async move {
            let resp = fut.await;
            state_clone
                .nb_requests_being_handled
                .fetch_sub(1, Ordering::Release);
            resp
        })
    }
}

impl Serialize for WorkerState {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("config", &self.config)?;

        map.serialize_entry(
            "nb_requests_received",
            &self.nb_requests_received.load(Ordering::Acquire),
        )?;
        map.serialize_entry(
            "nb_requests_being_handled",
            &self.nb_requests_being_handled.load(Ordering::Acquire),
        )?;
        map.end()
    }
}

// Simplified worker state, used when asking for the worker state
#[allow(dead_code)]
#[derive(Deserialize)]
pub(crate) struct DeserializedWorkerState {
    pub(crate) config: WorkerConfig,
    pub(crate) nb_requests_received: u32,
    pub(crate) nb_requests_being_handled: u32,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn worker_health_check_returns_200() {
        let http_client = reqwest::Client::builder()
            .build()
            .expect("Failed to build HTTP client");

        let addr = WorkerServer::create_and_run_worker().await.address;
        let response = http_client
            .get(&format!("http://{}/check-health", addr))
            .send()
            .await
            .expect("Failed to execute request");
        assert_eq!(response.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn worker_work_returns_200() {
        let addr = WorkerServer::create_and_run_worker().await.address;
        let http_client = reqwest::Client::builder()
            .build()
            .expect("Failed to build HTTP client");
        let json = json!({
            "duration": 10,
        });
        let response = http_client
            .post(&format!("http://{}/work", addr))
            .json(&json)
            .send()
            .await
            .expect("Failed to execute request");
        assert_eq!(response.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn worker_state_returns_200_and_correct_state() {
        let addr = WorkerServer::create_and_run_worker().await.address;
        let http_client = reqwest::Client::builder()
            .build()
            .expect("Failed to build HTTP client");

        let nb_request_sent = 10;
        for i in 1..=nb_request_sent {
            let response = http_client
                .get(&format!("http://{}/state", addr))
                .send()
                .await
                .expect("Failed to execute request");
            assert_eq!(response.status().as_u16(), 200);
            let state: DeserializedWorkerState = response
                .json()
                .await
                .expect("Failed to deserialize worker state");
            assert!(state.nb_requests_being_handled < nb_request_sent);
            assert_eq!(state.nb_requests_received, i);
            assert_eq!(state.config.address, addr);
        }
    }
}
