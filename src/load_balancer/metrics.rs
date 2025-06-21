use crate::load_balancer::{
    balancing_strategy::BalancingStrategy,
    server::{LoadBalancerError, LoadBalancerState},
};
use itertools::Itertools;
use rand::distr::weighted::WeightedIndex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;

#[derive(Debug)]
pub struct LoadBalancerStats {
    pub(crate) nb_requests_received: AtomicU64,
    pub(crate) nb_errors: AtomicU64,
    // Weight given to each worker
    pub(crate) workers_weight: RwLock<WeightedIndex<u64>>,
}

#[derive(Default, Debug)]
pub struct WorkerStatistics {
    pub nb_requests_sent: AtomicU64,
    pub nb_requests_handled: AtomicU64,
    pub nb_error_status_received: AtomicU64,
}

impl LoadBalancerStats {
    pub fn new(nb_workers: usize) -> Self {
        // Use uniform weight distribution by default
        let weights = vec![1; nb_workers];
        LoadBalancerStats {
            nb_requests_received: AtomicU64::default(),
            nb_errors: AtomicU64::default(),
            workers_weight: RwLock::new(
                WeightedIndex::new(weights)
                    .expect("Failed to create default worker weights distribution"),
            ),
        }
    }

    pub async fn recompute_weight(
        &self,
        state: &LoadBalancerState,
    ) -> Result<(), LoadBalancerError> {
        let strat = state.strategy.read().await;
        match *strat {
            BalancingStrategy::WeightedWorkersByErrors => {
                // Recompute weights every 100 requests
                if self.nb_requests_received.load(Ordering::Relaxed) % 100 == 0 {
                    // If a worker has a big error ratio its weight is going to be smaller
                    let weights = state
                        .workers
                        .iter()
                        .map(|worker| {
                            let nb_requests =
                                worker.stats.nb_requests_handled.load(Ordering::Relaxed);
                            let nb_errors = worker
                                .stats
                                .nb_error_status_received
                                .load(Ordering::Relaxed);

                            // If no requests have been handled yet give the default weight 1
                            if nb_requests == 0 {
                                100
                            } else {
                                // (the + 1 is to avoid being at zero and never receiveing a request anymore, especially if fail at first request)
                                (nb_requests - nb_errors) / nb_requests * 100 + 1
                            }
                        })
                        .collect_vec();
                    let mut write_lock = self.workers_weight.write().await;
                    let weights = WeightedIndex::new(weights)
                        .map_err(|e| LoadBalancerError::InternalError(e.into()))?;
                    *write_lock = weights;
                }
            }
            // Do not recompute weight for other strategies
            _ => {}
        }
        Result::<(), LoadBalancerError>::Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::atomic::Ordering, time::Duration};

    use futures::future::join_all;
    use itertools::Itertools;
    use serde_json::json;

    use crate::{
        load_balancer::server::{LoadBalancer, LoadBalancerState, WorkerView},
        worker::{WorkRequest, WorkerServer},
    };

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
            None,
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

    // Conditions
    // - the bigger the error ratio is, the less weight a worker should have
    // - workers with the similar error ratio should have similar weight
    // - weight should never be 0
    #[tokio::test]
    async fn weighted_workers_stats_are_computed_correctly() {
        let nb_workers = 5;
        let workers = WorkerView::test_workers(nb_workers);
        let worker_stats = workers.iter().map(|view| view.stats.clone()).collect_vec();
        let lb_state = LoadBalancerState::new(
            "127.0.0.1".to_owned(),
            workers.clone(),
            BalancingStrategy::WeightedWorkersByErrors,
        );

        // With 0 requests all weights should be equal
        lb_state.stats.recompute_weight(&lb_state).await.unwrap();
        let weights = lb_state.stats.workers_weight.read().await;
        assert!(weights.weights().count() == nb_workers as usize);
        assert!(weights.weights().all_equal());
        assert!(weights.weights().all(|weight| weight != 0));
        drop(weights);

        // Give all workers the same stats, all weights should be equal
        worker_stats
            .iter()
            .map(|stats| {
                stats.nb_requests_handled.store(100, Ordering::Relaxed);
                stats.nb_error_status_received.store(50, Ordering::Relaxed);
            })
            .collect_vec();
        lb_state.stats.recompute_weight(&lb_state).await.unwrap();
        let weights = lb_state.stats.workers_weight.read().await;
        assert!(weights.weights().count() == nb_workers as usize);
        assert!(weights.weights().all_equal());
        assert!(weights.weights().all(|weight| weight != 0));
        drop(weights);

        // Give all workers the same error ratio, all weights should be equal
        for (i, stats) in worker_stats.iter().enumerate() {
            stats
                .nb_requests_handled
                .store((i + 1) as u64 * 100, Ordering::Relaxed);
            stats
                .nb_error_status_received
                .store((i + 1) as u64 * 50, Ordering::Relaxed);
        }
        lb_state.stats.recompute_weight(&lb_state).await.unwrap();
        let weights = lb_state.stats.workers_weight.read().await;
        assert!(weights.weights().count() == nb_workers as usize);
        assert!(weights.weights().all_equal());
        assert!(weights.weights().all(|weight| weight != 0));
        drop(weights);

        // Workers with bigger error ratio have less weight
        for (i, stats) in worker_stats.iter().enumerate() {
            stats.nb_requests_handled.store(100, Ordering::Relaxed);
            stats
                .nb_error_status_received
                .store(100 - i as u64 * 10, Ordering::Relaxed);
        }
        lb_state.stats.recompute_weight(&lb_state).await.unwrap();
        let weights = lb_state.stats.workers_weight.read().await;
        assert!(weights.weights().is_sorted());
        assert!(weights.weights().count() == nb_workers as usize);
        assert!(weights.weights().all(|weight| weight != 0));
        drop(weights);
    }
}
