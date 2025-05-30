//! Worker server implementation using Axum

use axum::{Router, http::StatusCode, response::IntoResponse, routing::get, serve::Serve};
use color_eyre::Result;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

use crate::tracing::{make_span_with_request_id, on_request, on_response};

pub struct WorkerServer {
    pub address: String,
    server: Serve<TcpListener, Router, Router>,
}

impl WorkerServer {
    pub async fn build(address: &str) -> Result<Self> {
        let listener = TcpListener::bind(address).await?;
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
            address: address.to_string(),
            server,
        })
    }
    pub async fn run(self) -> Result<(), std::io::Error> {
        tracing::info!("Starting worker on {}", self.address);
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
    Ok(StatusCode::OK)
}

struct WorkerError(color_eyre::Report);
impl IntoResponse for WorkerError {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}
