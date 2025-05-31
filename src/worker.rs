//! Worker server implementation using Axum

use std::{path::Display, time::Duration};

use axum::{Router, http::StatusCode, response::IntoResponse, routing::get, serve::Serve};
use color_eyre::Result;
use thiserror::Error;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

use crate::tracing::{make_span_with_request_id, on_request, on_response};

pub struct WorkerServer {
    pub config: WorkerConfig,
    server: Serve<TcpListener, Router, Router>,
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub id: u8,
    pub address: String,
}

impl WorkerServer {
    pub async fn build(id: u8, address: &str) -> Result<Self> {
        let listener = TcpListener::bind(address).await?;
        let address = listener.local_addr()?.to_string();
        let router = Router::new()
            .route("/work", get(work))
            .route("/check-health", get(check_health))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(make_span_with_request_id)
                    .on_request(on_request)
                    .on_response(on_response),
            );
        let server = axum::serve(listener, router);
        Ok(WorkerServer {
            config: WorkerConfig { id, address },
            server,
        })
    }
    pub async fn run(self) -> Result<(), std::io::Error> {
        tracing::info!("Starting worker on {}", self.config.address);
        self.server.await
    }
}

/// Check health of the worker
#[tracing::instrument(name = "Health Check")]
pub async fn check_health() -> std::result::Result<impl IntoResponse, WorkerError> {
    Ok(StatusCode::OK)
}

/// Send work to worker
#[tracing::instrument(name = "Work")]
pub async fn work() -> std::result::Result<impl IntoResponse, WorkerError> {
    // Delay the response to simulate some work ongoing
    tokio::time::sleep(Duration::from_millis(10)).await;
    Ok(StatusCode::OK)
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn create_and_run_worker() -> String {
        let addr = "127.0.0.1:0";
        let worker = WorkerServer::build(0, addr)
            .await
            .expect("Failed to run server");
        let addr = worker.config.address.clone();
        tokio::spawn(worker.run());
        addr
    }

    #[tokio::test]
    async fn worker_health_check_returns_200() {
        let http_client = reqwest::Client::builder()
            .build()
            .expect("Failed to build HTTP client");

        let addr = create_and_run_worker().await;
        let response = http_client
            .get(&format!("http://{}/check-health", addr))
            .send()
            .await
            .expect("Failed to execute request");
        assert_eq!(response.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn worker_work_returns_200() {
        let addr = create_and_run_worker().await;
        let http_client = reqwest::Client::builder()
            .build()
            .expect("Failed to build HTTP client");
        let response = http_client
            .get(&format!("http://{}/work", addr))
            .send()
            .await
            .expect("Failed to execute request");
        assert_eq!(response.status().as_u16(), 200);
    }
}
