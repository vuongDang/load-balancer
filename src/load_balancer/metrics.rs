//! All the metrics collected by the load balancer
//! and also the statistics computed

use crate::worker::WorkerId;
use rand::distr::weighted::WeightedIndex;
use std::sync::atomic::AtomicU64;

#[derive(Debug)]
pub struct LoadBalancerStats {
    pub(crate) nb_requests_received: AtomicU64,
    pub(crate) nb_errors: AtomicU64,
    // Weight given to each worker
    pub(crate) workers_weight: WeightedIndex<WorkerId>,
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
            workers_weight: WeightedIndex::new(weights)
                .expect("Failed to create default worker weights distribution"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::atomic::Ordering, time::Duration};

    use futures::future::join_all;
    use itertools::Itertools;
    use serde_json::json;

    use crate::{
        load_balancer::server::{LoadBalancer, WorkerView},
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
}
