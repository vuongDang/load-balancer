use crate::worker::WorkerConfig;
use axum::{
    body::Body,
    extract::{Request, State},
    handler::Handler,
    http::StatusCode,
    response::{IntoResponse, Response},
};
// use hyper::Response;
use crate::load_balancer::balancing_strategy::BalancingStrategy;
use std::sync::Arc;
use thiserror::Error;
use tokio::{net::TcpListener, sync::RwLock};
use uuid::Uuid;
// use hyper::client::conn::http1;
// use hyper_util::rt::TokioIo;
// use tower::Service;

pub struct LoadBalancer {}

#[derive(Clone, Debug)]
pub struct LoadBalancerState {
    pub(crate) address: String,
    pub(crate) workers: Vec<WorkerConfig>,
    pub(crate) strategy: Arc<RwLock<BalancingStrategy>>,
}

impl LoadBalancer {
    pub async fn run(
        addr: &str,
        workers: Vec<WorkerConfig>,
        strategy: Option<BalancingStrategy>,
    ) -> Result<(), LoadBalancerError> {
        let listener = TcpListener::bind(addr).await?;
        let address = listener.local_addr()?.to_string();
        tracing::info!("Starting load balancer on {}", address);
        let state = LoadBalancerState {
            address,
            workers,
            strategy: Arc::new(RwLock::new(strategy.unwrap_or_default())),
        };
        let server = axum::serve(listener, handle_request.with_state(state));
        server.await.map_err(LoadBalancerError::IOError)
    }
}

/// Pick a worker server depending on the balancing strategy chosen and redirect the request to the worker
#[tracing::instrument(skip_all)]
pub async fn handle_request(
    State(state): State<LoadBalancerState>,
    request: Request<Body>,
) -> Result<impl IntoResponse, LoadBalancerError> {
    let request_id = Uuid::new_v4();
    tracing::trace!("[{}] received", request_id);

    let chosen_worker = BalancingStrategy::pick_worker(state).await;
    tracing::trace!("[{}] chosen worker {}", request_id, chosen_worker.id);

    // Send request to chosen worker
    let request = convert_axum_request_to_reqwest_request(request, chosen_worker);
    tracing::trace!("[{}] request sent {:?}", request_id, request);
    let response = reqwest::Client::new()
        .execute(request)
        .await
        .map_err(LoadBalancerError::WorkerError)?;
    tracing::trace!("[{}] response received {:?}", request_id, response);
    convert_reqwest_response_to_axum_response(response)
}

// Change uri to chosen worker and produces `reqwuest::Request`
fn convert_axum_request_to_reqwest_request(
    mut axum_req: Request<Body>,
    worker: WorkerConfig,
) -> reqwest::Request {
    // Create a new uri with the worker authority
    let uri = axum_req.uri_mut();
    *uri = http::uri::Builder::new()
        .scheme("http")
        .authority(worker.address.clone())
        .path_and_query(uri.path_and_query().expect("No path found").to_string())
        .build()
        .unwrap();
    let req = axum_req.map(|body| reqwest::Body::wrap_stream(body.into_data_stream()));
    reqwest::Request::try_from(req).expect("http::Uri to url::Url conversion failed")
}

/// Copied from  https://github.com/tokio-rs/axum/blob/7eabf7e645caf4e8e974cbdd886ab99eb2766d1d/examples/reqwest-response/src/main.rs#L62C1-L68C2
fn convert_reqwest_response_to_axum_response(
    response: reqwest::Response,
) -> Result<impl IntoResponse, LoadBalancerError> {
    let mut response_builder = Response::builder().status(response.status());
    *response_builder.headers_mut().unwrap() = response.headers().clone();
    response_builder
        .body(Body::from_stream(response.bytes_stream()))
        .map_err(|e| LoadBalancerError::InternalError(e.into()))
}

#[derive(Error, Debug)]
pub enum LoadBalancerError {
    #[error("internal error: {0}")]
    InternalError(color_eyre::Report),
    #[error(transparent)]
    WorkerError(#[from] reqwest::Error),
    #[error(transparent)]
    IOError(#[from] std::io::Error),
}

impl IntoResponse for LoadBalancerError {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("LoadBalancer error: {}", self),
        )
            .into_response()
    }
}
