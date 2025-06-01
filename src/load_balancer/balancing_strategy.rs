use super::server::LoadBalancerState;
use crate::worker::{WorkerConfig, WorkerId};
use std::{collections::HashMap, sync::Arc};

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
use rand::Rng;
use tokio::sync::Mutex;

impl BalancingStrategy {
    pub async fn pick_worker(state: LoadBalancerState) -> WorkerConfig {
        let LoadBalancerState {
            address: _,
            workers,
            strategy,
        } = state;

        let strategy = strategy.read().await;

        // Pick a server worker depending on the balancing strategy
        let chosen_worker = match &*strategy {
            RoundRobin(last_worker) => round_robin(workers, last_worker).await,
            Random => random(workers).await,
            LeastConnection => unimplemented!(),
            ResourceBased => unimplemented!(),
        };

        chosen_worker
    }
}

async fn round_robin(
    workers: Vec<WorkerConfig>,
    last_worker: &Arc<Mutex<WorkerId>>,
) -> WorkerConfig {
    let mut id = last_worker.lock().await;
    *id = (*id + 1u8) % workers.len() as u8;
    let worker = workers
        .iter()
        .find(|worker| worker.id == *id)
        .expect("Worker not found");
    worker.clone()
}

async fn random(workers: Vec<WorkerConfig>) -> WorkerConfig {
    let mut rng = rand::rng();
    let id = rng.random_range(0..workers.len() as u8);
    let worker = workers
        .iter()
        .find(|worker| worker.id == id)
        .expect("Worker not found");
    worker.clone()
}

// Pick the worker with least request givent
async fn least_connection(
    worker: Vec<WorkerConfig>,
    connection_history: &Arc<Mutex<HashMap<WorkerId, u64>>>,
) -> WorkerConfig {
    worker.first().unwrap().clone()
}
