use crate::load_balancer::balancing_strategy::BalancingStrategy;
use crate::worker::WorkerConfig;
use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    serve::Serve,
};
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use thiserror::Error;
use tokio::{net::TcpListener, sync::RwLock};
use tracing::{error, info, trace};
use uuid::Uuid;

pub struct LoadBalancer {
    #[allow(dead_code)]
    pub address: String,
    server: Serve<TcpListener, Router, Router>,
    // We don't need lock here since the only field that could be changed
    // is already behind a lock
    #[allow(dead_code)]
    pub state: LoadBalancerState,
}

/// View of the worker from the LoadBalancer pov
#[derive(Debug, Clone)]
pub struct WorkerView {
    pub config: WorkerConfig,
    pub stats: Arc<WorkerStatistics>,
}

#[derive(Default, Debug)]
pub struct WorkerStatistics {
    pub nb_requests_sent: AtomicU64,
    pub nb_requests_handled: AtomicU64,
    pub nb_error_status_received: AtomicU64,
}

#[derive(Clone, Debug)]
pub struct LoadBalancerState {
    #[allow(dead_code)]
    pub(crate) address: String,
    // The worker servers available to the load balancer
    pub(crate) workers: Vec<WorkerView>,
    // The balancing strategy being currently employed
    pub(crate) strategy: Arc<RwLock<BalancingStrategy>>,
    pub(crate) stats: Arc<LoadBalancerStats>,
}

#[derive(Debug, Default)]
pub struct LoadBalancerStats {
    pub(crate) nb_requests_received: AtomicU64,
    pub(crate) nb_errors: AtomicU64,
}

impl LoadBalancer {
    pub async fn build(
        addr: &str,
        workers: Vec<WorkerConfig>,
        strategy: Option<BalancingStrategy>,
    ) -> Result<Self, LoadBalancerError> {
        let listener = TcpListener::bind(addr).await?;
        let address = listener.local_addr()?.to_string();
        let worker_views = workers
            .into_iter()
            .map(|config| WorkerView {
                config,
                stats: Arc::new(WorkerStatistics::default()),
            })
            .collect_vec();
        let state = LoadBalancerState {
            address: address.clone(),
            workers: worker_views,
            strategy: Arc::new(RwLock::new(strategy.unwrap_or_default())),
            stats: Arc::new(LoadBalancerStats::default()),
        };
        let router = LoadBalancer::router(state.clone());
        let server = axum::serve(listener, router);

        Ok(LoadBalancer {
            address,
            server,
            state,
        })
    }

    pub async fn run(self) -> Result<(), LoadBalancerError> {
        tracing::info!("Starting load balancer on {}", self.address);
        self.server.await.map_err(LoadBalancerError::IOError)
    }

    // To avoid duplication from real implementation and test case
    fn router(state: LoadBalancerState) -> Router {
        Router::new()
            .route("/balancing-strategy", post(set_balancing_strategy))
            .route("/{*key}", get(transfer_request).post(transfer_request))
            .with_state(state)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SetBalancingStrategyRequest {
    pub strategy: BalancingStrategy,
}

#[tracing::instrument(skip_all)]
pub async fn set_balancing_strategy(
    State(state): State<LoadBalancerState>,
    Json(request): Json<SetBalancingStrategyRequest>,
) -> std::result::Result<impl IntoResponse, LoadBalancerError> {
    let mut s = state.strategy.write().await;
    *s = request.strategy;
    Ok(StatusCode::OK)
}

/// Pick a worker server depending on the balancing strategy chosen and redirect the request to the worker
#[tracing::instrument(skip_all)]
pub async fn transfer_request(
    State(state): State<LoadBalancerState>,
    request: Request<Body>,
) -> Result<impl IntoResponse, LoadBalancerError> {
    state
        .stats
        .nb_requests_received
        .fetch_add(1, Ordering::Relaxed);

    let request_id = Uuid::new_v4();

    let chosen_worker = BalancingStrategy::pick_worker(&state).await?;
    trace!("[{}] chosen worker {}", request_id, chosen_worker.config.id);

    // Send request to chosen worker
    let request = convert_axum_request_to_reqwest_request(request, &chosen_worker.config);
    trace!("[{}] request sent {:?}", request_id, request);

    chosen_worker
        .stats
        .nb_requests_sent
        .fetch_add(1, Ordering::Relaxed);

    let response = reqwest::Client::new().execute(request).await;

    chosen_worker
        .stats
        .nb_requests_handled
        .fetch_add(1, Ordering::Relaxed);

    // The load balancer fails to send the request
    if response.is_err() {
        error!("[{}] response received {:?}", request_id, response);
        state.stats.nb_errors.fetch_add(1, Ordering::Relaxed);
        chosen_worker
            .stats
            .nb_error_status_received
            .fetch_add(1, Ordering::Relaxed);
    }
    let response = response.map_err(|e| LoadBalancerError::InternalError(e.into()))?;

    // The load balancer has encountered an error
    if response.status().is_client_error() {
        error!("[{}] response received {:?}", request_id, response);
        state.stats.nb_errors.fetch_add(1, Ordering::Relaxed);
    // Worker server returns an error, increment the worker error stats
    } else if response.status().is_server_error() {
        info!(
            "[{}] Worker server encountered an error: {:?}",
            request_id, response
        );
        chosen_worker
            .stats
            .nb_error_status_received
            .fetch_add(1, Ordering::Relaxed);
    // The request was successful
    } else if response.status().is_success() {
        trace!("[{}] response received {:?}", request_id, response);
    }
    convert_reqwest_response_to_axum_response(response)
}

// Change uri to chosen worker and produces `reqwuest::Request`
fn convert_axum_request_to_reqwest_request(
    mut axum_req: Request<Body>,
    worker: &WorkerConfig,
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

impl LoadBalancer {
    /// When we need control on the strategy from the outside
    /// Meant to be used for tests
    #[cfg(test)]
    async fn test_run(
        addr: &str,
        workers: Vec<WorkerConfig>,
    ) -> Result<LoadBalancerState, LoadBalancerError> {
        let load_balancer = LoadBalancer::build(addr, workers, None).await?;
        let state = load_balancer.state.clone();
        let _ = tokio::spawn(load_balancer.run());
        Ok(state)
    }

    /// When we need control on the strategy from the outside
    /// Meant to be used for tests
    #[cfg(test)]
    async fn test_run_2(
        addr: &str,
        workers: Vec<WorkerView>,
    ) -> Result<LoadBalancerState, LoadBalancerError> {
        let load_balancer =
            LoadBalancer::build(addr, workers.into_iter().map(|w| w.config).collect(), None)
                .await?;
        let state = load_balancer.state.clone();
        let _ = tokio::spawn(load_balancer.run());
        Ok(state)
    }
}

impl WorkerView {
    #[cfg(test)]
    pub(crate) fn test_workers(nb_workers: u8) -> Vec<WorkerView> {
        let mut workers = vec![];
        for _ in 0..nb_workers {
            workers.push(WorkerView {
                config: WorkerConfig::test_config(),
                stats: Arc::new(WorkerStatistics::default()),
            });
        }
        workers
    }

    pub fn new(config: WorkerConfig) -> Self {
        WorkerView {
            config,
            stats: Arc::new(WorkerStatistics::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::atomic::AtomicUsize, time::Duration};

    use crate::worker::{WorkRequest, WorkerServer};

    use super::*;
    use futures::future::join_all;
    use itertools::Itertools;
    use rand::seq::IndexedRandom;
    use serde_json::json;
    use strum::IntoEnumIterator;

    #[tokio::test]

    async fn worker_requests_are_redirected() {
        // We start 3 workers
        let mut workers = vec![];
        for _ in 0..3 {
            let worker = WorkerServer::create_and_run_worker().await;
            workers.push(worker);
        }

        // We start the loadbalancer
        let load_balancer_state = LoadBalancer::test_run("127.0.0.1:0", workers.clone())
            .await
            .unwrap();
        let http_client = reqwest::Client::builder()
            .build()
            .expect("Failed to build HTTP client");

        // Test the worker different endpoints
        let get_endpoints = vec!["check-health", "state"];
        let post_endpoints = vec!["work"];

        for endpoint in get_endpoints {
            let response = http_client
                .get(&format!(
                    "http://{}/{}",
                    load_balancer_state.address, endpoint
                ))
                .send()
                .await
                .expect("Failed to execute request");
            assert_eq!(response.status().as_u16(), 200);
        }

        for endpoint in post_endpoints {
            let json = json!(WorkRequest { duration: 1 });

            let response = http_client
                .post(&format!(
                    "http://{}/{}",
                    load_balancer_state.address, endpoint
                ))
                .json(&json)
                .send()
                .await
                .expect("Failed to execute request");
            assert_eq!(response.status().as_u16(), 200);
        }
    }

    #[tokio::test]
    async fn error_statistics_are_taken_correctly() {
        // let _ = crate::tracing::init_tracing("trace");
        // We have 1 worker
        let workers = WorkerView::test_workers(2);

        // We start a loadbalancer without workers actually running
        let load_balancer = LoadBalancer::build(
            "127.0.0.1:0",
            workers
                .iter()
                .map(|worker| worker.config.clone())
                .collect_vec(),
            Some(BalancingStrategy::RoundRobin(AtomicUsize::default())),
        )
        .await
        .unwrap();
        let worker_stats = load_balancer
            .state
            .workers
            .iter()
            .map(|worker| worker.stats.clone())
            .collect_vec();
        let lb_stats = load_balancer.state.stats.clone();
        let lb_address = load_balancer.address.clone();
        tokio::spawn(load_balancer.run());

        let http_client = reqwest::Client::builder()
            .build()
            .expect("Failed to build HTTP client");

        // Send two failing requests
        for i in 0..2 {
            let response = http_client
                .get(&format!("http://{}/{}", lb_address, "check-health"))
                .send()
                .await
                .expect("Failed to execute request");
            assert!(response.status().is_server_error());
            assert_eq!(
                worker_stats[i]
                    .nb_error_status_received
                    .load(Ordering::Relaxed),
                1
            );
            assert_eq!(worker_stats[i].nb_requests_sent.load(Ordering::Relaxed), 1);
            assert_eq!(
                worker_stats[i].nb_requests_handled.load(Ordering::Relaxed),
                1
            );
            assert_eq!(
                lb_stats.nb_requests_received.load(Ordering::Relaxed),
                i as u64 + 1
            );
        }
        assert_eq!(lb_stats.nb_errors.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn success_statistics_are_taken_correctly() {
        // Start a new server and start the new workers
        let nb_workers = 4;
        let workers =
            join_all((0..nb_workers).map(|_| WorkerServer::create_and_run_worker())).await;
        let lb_state = LoadBalancer::test_run("127.0.0.1:0", workers)
            .await
            .unwrap();
        let worker_stats = lb_state
            .workers
            .iter()
            .map(|worker| worker.stats.clone())
            .collect_vec();
        let lb_stats = lb_state.stats;

        let http_client = reqwest::Client::builder()
            .build()
            .expect("Failed to build HTTP client");

        // Send succesfull requests anc check stats
        for i in 0..nb_workers {
            let _response = http_client
                .get(&format!("http://{}/{}", lb_state.address, "check-health"))
                .send()
                .await
                .expect("Failed to execute request");
            assert_eq!(
                worker_stats[i]
                    .nb_error_status_received
                    .load(Ordering::Relaxed),
                0
            );
            assert_eq!(worker_stats[i].nb_requests_sent.load(Ordering::Relaxed), 1);
            assert_eq!(
                worker_stats[i].nb_requests_handled.load(Ordering::Relaxed),
                1
            );
            assert_eq!(
                lb_stats.nb_requests_received.load(Ordering::Relaxed),
                i as u64 + 1
            );
            assert_eq!(lb_stats.nb_errors.load(Ordering::Relaxed), 0);
        }

        for _ in 0..nb_workers {
            // Send a long request to the server that takes 10 seconds
            let json = json!(WorkRequest { duration: 20000 });
            tokio::spawn(
                http_client
                    .post(&format!("http://{}/{}", lb_state.address, "work"))
                    .json(&json)
                    .send(),
            );
        }

        // Wait a bit to let the load balancer have time to send the request to the worker server
        tokio::time::sleep(Duration::from_millis(1000)).await;

        for i in 0..nb_workers {
            assert_eq!(
                worker_stats[i]
                    .nb_error_status_received
                    .load(Ordering::Relaxed),
                0
            );
            assert_eq!(worker_stats[i].nb_requests_sent.load(Ordering::Relaxed), 2);
            assert_eq!(
                worker_stats[i].nb_requests_handled.load(Ordering::Relaxed),
                1
            );
        }
        assert_eq!(
            lb_stats.nb_requests_received.load(Ordering::Relaxed),
            nb_workers as u64 * 2
        );
        assert_eq!(lb_stats.nb_errors.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn change_strategy_endpoint_works_as_expected() {
        let workers = WorkerConfig::test_workers(3);
        let load_balancer_state = LoadBalancer::test_run("127.0.0.1:0", workers.clone())
            .await
            .unwrap();
        let http_client = reqwest::Client::builder()
            .build()
            .expect("Failed to build HTTP client");

        let possible_strategies = BalancingStrategy::iter().collect_vec();

        for _ in 0..10 {
            let strategy = possible_strategies.choose(&mut rand::rng()).unwrap();
            let json = json!({
                "strategy": strategy,
            });
            let response = http_client
                .post(&format!(
                    "http://{}/balancing-strategy",
                    load_balancer_state.address
                ))
                .json(&json)
                .send()
                .await
                .expect("Failed to execute request");
            assert_eq!(response.status().as_u16(), 200);
            let lock = load_balancer_state.strategy.read().await;
            assert_eq!(*strategy, *lock);
        }
    }
}
