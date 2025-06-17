use super::server::{LoadBalancerError, LoadBalancerState};
use crate::{
    load_balancer::server::WorkerView,
    worker::{DeserializedWorkerState, WorkerConfig},
};
use BalancingStrategy::*;
use color_eyre::eyre::eyre;
use rand::seq::IndexedRandom;
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicUsize;
use strum_macros::AsRefStr;
pub(crate) use strum_macros::EnumIter;

#[derive(Debug, EnumIter, AsRefStr)]
pub enum BalancingStrategy {
    RoundRobin(AtomicUsize),
    LeastConnectionWithStatsFromWorkers,
    LeastConnectionWithInternalStats,
    Random,
    ResourceBased,
}

impl Default for BalancingStrategy {
    fn default() -> Self {
        RoundRobin(AtomicUsize::default())
    }
}

impl BalancingStrategy {
    pub async fn pick_worker(state: &LoadBalancerState) -> Result<&WorkerView, LoadBalancerError> {
        let LoadBalancerState {
            address: _,
            workers,
            strategy,
            stats: _,
        } = state;

        let strategy = strategy.read().await;

        // Pick a server worker depending on the balancing strategy
        let chosen_worker = match &*strategy {
            RoundRobin(last_worker) => round_robin(&workers, last_worker).await?,
            Random => random(&workers).await?,
            LeastConnectionWithStatsFromWorkers => {
                least_connection_with_stats_from_workers(&workers).await?
            }
            LeastConnectionWithInternalStats => {
                least_connection_with_internal_stats(&workers).await?
            }
            ResourceBased => unimplemented!(),
        };

        Ok(chosen_worker)
    }
}

async fn round_robin<'a>(
    workers: &'a Vec<WorkerView>,
    last_worker: &AtomicUsize,
) -> Result<&'a WorkerView, LoadBalancerError> {
    let last_worker_index =
        last_worker.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as usize % workers.len();
    let next_worker = &workers[last_worker_index];
    Ok(next_worker)
}

async fn random(workers: &Vec<WorkerView>) -> Result<&WorkerView, LoadBalancerError> {
    let next_worker = workers
        .choose(&mut rand::rng())
        .ok_or(LoadBalancerError::InternalError(eyre!(
            "List of workers is empty"
        )))?;
    Ok(next_worker)
}

// Pick the worker who is handling the least number of requests, with stats from the workers
async fn least_connection_with_stats_from_workers(
    workers: &Vec<WorkerView>,
) -> Result<&WorkerView, LoadBalancerError> {
    let requests = workers
        .iter()
        .map(|view| request_worker_state(&view.config));
    // Request workers states concurrently
    let requests_response = futures::future::join_all(requests).await;

    // Filter out workers which did not sent their state
    let worker_states = requests_response
        .iter()
        .filter_map(|worker| worker.as_ref().ok());

    // Find worker with least request being handled
    let worker_id_with_min_request_being_handled = worker_states
        .min_by_key(|state| state.nb_requests_being_handled)
        .ok_or(LoadBalancerError::InternalError(eyre!(
            "No stats received from any worker"
        )))?
        .config
        .id;

    Ok(workers
        .iter()
        .find(|worker| worker.config.id == worker_id_with_min_request_being_handled)
        .ok_or(LoadBalancerError::InternalError(eyre!(
            "Worker not found in the load balancer config"
        )))?)
}

// Pick the worker who is handling the least number of requests, with stats from the load balancer
async fn least_connection_with_internal_stats(
    workers: &[WorkerView],
) -> Result<&WorkerView, LoadBalancerError> {
    let worker_with_min_request_being_handled = workers
        .iter()
        .min_by_key(|view| {
            let nb_requests_handled = view
                .stats
                .as_ref()
                .nb_requests_handled
                .load(std::sync::atomic::Ordering::Relaxed);
            let nb_requests_sent = view
                .stats
                .as_ref()
                .nb_requests_sent
                .load(std::sync::atomic::Ordering::Relaxed);
            nb_requests_sent - nb_requests_handled
        })
        .ok_or(LoadBalancerError::InternalError(eyre!(
            "No workers in config"
        )))?;
    Ok(worker_with_min_request_being_handled)
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
            (LeastConnectionWithStatsFromWorkers, LeastConnectionWithStatsFromWorkers) => true,
            (LeastConnectionWithInternalStats, LeastConnectionWithInternalStats) => true,
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
            LeastConnectionWithStatsFromWorkers => {
                serializer.serialize_str("LeastConnectionWithStatsFromWorkers")
            }
            LeastConnectionWithInternalStats => {
                serializer.serialize_str("LeastConnectionWithInternalStats")
            }
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
            "LeastConnectionWithInternalStats" => {
                Ok(BalancingStrategy::LeastConnectionWithInternalStats)
            }
            "LeastConnectionWithStatsFromWorkers" => {
                Ok(BalancingStrategy::LeastConnectionWithStatsFromWorkers)
            }
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
    use itertools::Itertools;
    use serde_json::json;

    use super::*;
    use crate::worker::WorkerServer;

    #[tokio::test]
    async fn round_robin_picks_next_worker() {
        let workers = WorkerView::test_workers(5);
        let glob_index = AtomicUsize::default();
        for index in 0..10 {
            assert_eq!(
                round_robin(&workers, &glob_index).await.unwrap().config.id,
                workers[index % workers.len()].config.id
            );
        }
    }

    #[tokio::test]
    async fn random_does_not_create_impossible_value() {
        let workers = WorkerView::test_workers(5);
        for _ in 1..=10 {
            let next_worker = random(&workers).await.expect("should not fail");
            assert!(
                workers
                    .iter()
                    .find(|worker| worker.config == next_worker.config)
                    .is_some()
            );
        }
    }

    #[tokio::test]
    async fn least_connection_with_internal_stats_picks_the_right_worker() {
        let workers = WorkerView::test_workers(5);
        for i in 0..=10 {
            let next_worker = least_connection_with_internal_stats(&workers)
                .await
                .expect("should not fail");
            let index = i % workers.len();
            println!("{}", index);
            let expected_worker = workers.get(index).unwrap();
            assert_eq!(next_worker.config, expected_worker.config);
            expected_worker
                .stats
                .nb_requests_sent
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn least_connection_with_worker_state_picks_the_right_worker() {
        const NB_WORKERS: usize = 3;
        let workers =
            (0..NB_WORKERS).map(|_| async { WorkerServer::create_and_run_worker().await });
        let worker_configs = join_all(workers).await;
        let workers = worker_configs
            .into_iter()
            .map(WorkerView::new)
            .collect_vec();

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
                    .post(&format!("http://{}/work", worker.config.address))
                    .json(&json)
                    .send()
                    .await
                    .expect("Failed to execute request");

                assert_eq!(response.status().as_u16(), 200);
            });
        }

        let next_worker = least_connection_with_stats_from_workers(&workers)
            .await
            .expect("Failed to find next worker");
        assert_eq!(next_worker.config, workers.last().unwrap().config)
    }
}
