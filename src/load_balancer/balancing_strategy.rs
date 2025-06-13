use super::server::{LoadBalancerError, LoadBalancerState};
use crate::worker::{DeserializedWorkerState, WorkerConfig};
use BalancingStrategy::*;
use color_eyre::eyre::eyre;
use rand::seq::IndexedRandom;
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicUsize;
pub(crate) use strum_macros::EnumIter;

#[derive(Default, Debug, EnumIter)]
pub enum BalancingStrategy {
    RoundRobin(AtomicUsize),
    LeastConnection,
    #[default]
    Random,
    ResourceBased,
}

impl BalancingStrategy {
    pub async fn pick_worker(state: &LoadBalancerState) -> Result<WorkerConfig, LoadBalancerError> {
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
    last_worker: &AtomicUsize,
) -> Result<WorkerConfig, LoadBalancerError> {
    let last_worker_index =
        last_worker.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as usize % workers.len();
    let next_worker = &workers[last_worker_index];
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

impl PartialEq for BalancingStrategy {
    fn eq(&self, other: &Self) -> bool {
        use BalancingStrategy::*;
        match (self, other) {
            (RoundRobin(_), RoundRobin(_)) => true,
            (LeastConnection, LeastConnection) => true,
            (Random, Random) => true,
            (ResourceBased, ResourceBased) => true,
            _ => false,
        }
    }
}

impl Serialize for BalancingStrategy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use BalancingStrategy::*;
        match self {
            RoundRobin(_) => serializer.serialize_str("RoundRobin"),
            LeastConnection => serializer.serialize_str("LeastConnection"),
            Random => serializer.serialize_str("Random"),
            ResourceBased => serializer.serialize_str("ResourceBased"),
        }
    }
}

impl<'de> Deserialize<'de> for BalancingStrategy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "RoundRobin" => Ok(BalancingStrategy::RoundRobin(AtomicUsize::default())),
            "LeastConnection" => Ok(BalancingStrategy::LeastConnection),
            "Random" => Ok(BalancingStrategy::Random),
            "ResourceBased" => Ok(BalancingStrategy::ResourceBased),
            _ => Err(serde::de::Error::custom(format!(
                "Unknown balancing strategy: {}",
                s
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::future::join_all;
    use serde_json::json;

    use super::*;
    use crate::worker::{WorkerConfig, WorkerServer};

    #[tokio::test]
    async fn round_robin_picks_next_worker() {
        let workers = WorkerConfig::test_workers(5);
        let glob_index = AtomicUsize::default();
        for index in 0..10 {
            assert_eq!(
                round_robin(&workers, &glob_index).await.unwrap().id,
                workers[index % workers.len()].id
            );
        }
    }

    #[tokio::test]
    async fn random_does_not_create_impossible_value() {
        let workers = WorkerConfig::test_workers(5);
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
