use super::server::{LoadBalancerError, LoadBalancerState};
use crate::worker::{DeserializedWorkerState, WorkerConfig, WorkerId, work};
use std::{ops::Index, sync::Arc};

#[derive(Default, Debug)]
pub enum BalancingStrategy {
    // TODO: maybe use an atomic
    RoundRobin(Arc<Mutex<WorkerId>>),
    LeastConnection,
    #[default]
    Random,
    ResourceBased,
}
use BalancingStrategy::*;
use color_eyre::eyre::eyre;
use itertools::Itertools;
use rand::{Rng, seq::IndexedRandom};
use tokio::sync::Mutex;

impl BalancingStrategy {
    pub async fn pick_worker(state: LoadBalancerState) -> Result<WorkerConfig, LoadBalancerError> {
        let LoadBalancerState {
            address: _,
            workers,
            strategy,
        } = state;

        let strategy = strategy.read().await;

        // Pick a server worker depending on the balancing strategy
        let chosen_worker = match &*strategy {
            RoundRobin(last_worker) => round_robin(&workers, last_worker).await?,
            Random => random(&workers).await?,
            LeastConnection => least_connection(&workers).await?,
            ResourceBased => unimplemented!(),
        };

        Ok(chosen_worker)
    }
}

async fn round_robin(
    workers: &Vec<WorkerConfig>,
    last_worker: &Arc<Mutex<WorkerId>>,
) -> Result<WorkerConfig, LoadBalancerError> {
    let last_worker_id = *last_worker.lock().await;
    let last_worker_index = workers
        .iter()
        .position(|worker| last_worker_id == worker.id)
        .ok_or(LoadBalancerError::InternalError(eyre!(
            "Last worker id is not in the available workers"
        )))?;
    let next_worker = if let Some(worker) = workers.get(last_worker_index + 1) {
        worker
    } else {
        workers
            .first()
            .ok_or(LoadBalancerError::InternalError(eyre!(
                "No available workers"
            )))?
    };
    *last_worker.lock().await = next_worker.id;
    Ok(next_worker.clone())
}

async fn random(workers: &Vec<WorkerConfig>) -> Result<WorkerConfig, LoadBalancerError> {
    let next_worker = workers
        .choose(&mut rand::rng())
        .ok_or(LoadBalancerError::InternalError(eyre!(
            "List of workers is empty"
        )))?;
    Ok(next_worker.clone())
}

// Pick the worker who is handling the least number of requests
async fn least_connection(workers: &Vec<WorkerConfig>) -> Result<WorkerConfig, LoadBalancerError> {
    let requests = workers.iter().map(request_worker_state);
    // Request workers states concurrently
    let requests_response = futures::future::join_all(requests).await;

    // Filter out workers which did not sent their state
    let worker_states = requests_response
        .iter()
        .filter_map(|worker| worker.as_ref().ok());

    // Find worker with least request being handled
    let state_with_min_request_being_handled = worker_states
        .min_by_key(|state| state.nb_requests_being_handled)
        .ok_or(LoadBalancerError::InternalError(eyre!(
            "No stats received from any worker"
        )))?;
    Ok(state_with_min_request_being_handled.config.clone())
}

// Make a request to a worker about its state
async fn request_worker_state(
    worker: &WorkerConfig,
) -> Result<DeserializedWorkerState, LoadBalancerError> {
    let http_client = reqwest::Client::builder()
        .build()
        .map_err(|e| LoadBalancerError::InternalError(e.into()))?;
    let response = http_client
        .get(&format!("http://{}/state", worker.address))
        .send()
        .await
        .map_err(|e| LoadBalancerError::WorkerError(e.into()))?;
    let state: DeserializedWorkerState = response
        .json()
        .await
        .map_err(|e| LoadBalancerError::InternalError(e.into()))?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use futures::future::join_all;
    use serde_json::json;

    use super::*;
    use crate::worker::{WorkerConfig, WorkerServer};

    // Worker config with random ids
    fn test_worker_configs(nb_workers: u8) -> Vec<WorkerConfig> {
        let mut workers = vec![];
        for _ in 0..nb_workers {
            let id = rand::rng().random::<u32>();

            workers.push(WorkerConfig {
                id,
                address: "127.0.0.1:0".to_string(),
            });
        }
        workers
    }

    #[tokio::test]
    async fn round_robin_picks_next_worker() {
        let workers = test_worker_configs(5);
        let init_worker = Arc::new(Mutex::new(workers[0].id));
        for index in 1..=10 {
            assert_eq!(
                round_robin(&workers, &init_worker).await.unwrap().id,
                workers[index % workers.len()].id
            );
        }
    }

    #[tokio::test]
    async fn random_does_not_create_impossible_value() {
        let workers = test_worker_configs(5);
        for _ in 1..=10 {
            let next_worker = random(&workers).await.expect("should not fail");
            assert!(
                workers
                    .iter()
                    .find(|worker| **worker == next_worker)
                    .is_some()
            );
        }
    }

    #[tokio::test]
    async fn least_connection_picks_the_right_worker() {
        const NB_WORKERS: usize = 3;
        let workers =
            (0..NB_WORKERS).map(|_| async { WorkerServer::create_and_run_worker().await });
        let workers = join_all(workers).await;

        // Send long work to all workers except last one
        for i in 0..(NB_WORKERS - 1) {
            let worker = workers.get(i).unwrap().clone();
            tokio::spawn(async move {
                let http_client = reqwest::Client::builder()
                    .build()
                    .expect("Failed to build HTTP client");
                let json = json!({
                    "duration": 1000, // we make them work for 1s
                });
                let response = http_client
                    .post(&format!("http://{}/work", worker.address))
                    .json(&json)
                    .send()
                    .await
                    .expect("Failed to execute request");

                assert_eq!(response.status().as_u16(), 200);
            });
        }

        let next_worker = least_connection(&workers)
            .await
            .expect("Failed to find next worker");
        assert_eq!(&next_worker, workers.last().unwrap())
    }
}
