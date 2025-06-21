#![allow(unused)]

use std::{
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};

use criterion::{Criterion, criterion_group, criterion_main};
use futures::future::join_all;
use load_balancer::{
    load_balancer::{
        balancing_strategy::BalancingStrategy,
        server::{LoadBalancer, SetBalancingStrategyRequest},
    },
    worker::{self, WorkerConfig, WorkerServer},
};
use serde_json::json;
use strum::IntoEnumIterator;
use tokio::{sync::Mutex, task::JoinSet};

// Number of worker servers available
static NB_WORKERS: u32 = 10;
// Number of requests sent to the load balancer
static NB_REQUESTS: u64 = 1;
// If the work duration is not set, pick a random value within thi srange
static RANDOM_WORK_DURATION_RANGE: std::ops::Range<u64> = 0..2000;
// The time the worker server spends on working
static WORK_DURATION_MS: u64 = 1000;
// IP of the load balancer
static IP: &'static str = "127.0.0.1";

criterion_group!(benches, bench_balancing_strategies);
criterion_main!(benches);

fn bench_balancing_strategies(c: &mut Criterion) {
    load_balancer::tracing::init_tracing("error").expect("Failed to init tracing");
    let nb_requests = NB_REQUESTS;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    // Start the load balancer and worker servers
    let lb_address = rt.block_on(setup_workers_and_lb());

    // Benchmarks where the work duration of requests are constants
    let mut group = c.benchmark_group("Balancing Strategies with constant work duration");
    for strat in BalancingStrategy::iter() {
        let strat_name = strat_copy(&strat).as_ref().to_owned();
        group.bench_function(strat_name, |b| {
            b.iter(|| {
                let strat_copy = strat_copy(&strat);
                let lb = (&lb_address).clone();
                rt.block_on(async move {
                    send_requests_to_load_balancer(
                        lb,
                        nb_requests,
                        Some(WORK_DURATION_MS),
                        strat_copy,
                    )
                });
            })
        });
    }
    group.finish();

    // Benchmarks where the work duration of requests are random
    let mut group = c.benchmark_group("Balancing Strategies with random work duration");
    for strat in BalancingStrategy::iter() {
        let strat_name = strat_copy(&strat).as_ref().to_owned();
        group.bench_function(strat_name, |b| {
            b.iter(|| {
                let strat_copy = strat_copy(&strat);
                let lb = (&lb_address).clone();
                rt.block_on(async move {
                    send_requests_to_load_balancer(lb, nb_requests, None, strat_copy)
                });
            })
        });
    }

    group.finish();
}

async fn setup_workers_and_lb() -> String {
    // Start all worker servers
    let workers_setup =
        join_all((0..NB_WORKERS).map(|_| async { WorkerServer::create_and_run_worker().await }));
    let workers = workers_setup.await;

    // Start load balancer
    let address = format!("{}:0", IP);
    let load_balancer = LoadBalancer::build(&address, workers, None).await.unwrap();
    let address = load_balancer.address.clone();
    tokio::spawn(load_balancer.run());
    address
}

// Setup the loadbalancer with the provided strategy then send `nb_requests` requests
// at the "/work" endpoint
// If work duration is None then picks a random value
async fn send_requests_to_load_balancer(
    load_balancer_address: String,
    nb_requests: u64,
    work_duration: Option<u64>,
    strategy: BalancingStrategy,
) {
    // reqwuest http client to send request to the load balancer
    let http_client = reqwest::Client::builder()
        .build()
        .expect("Failed to build HTTP client");

    // Set the load balancer strategy for this bench
    let lb_address = load_balancer_address.clone();
    let json = json!(SetBalancingStrategyRequest { strategy });
    let response = &http_client
        .post(&format!("http://{}/balancing-strategy", lb_address))
        .json(&json)
        .send()
        .await
        .expect("Failed to execute request");

    // Send requests as a `JoinSet` to wait for all of them
    let mut requests = JoinSet::new();
    for _ in 0..nb_requests {
        // Pick a random work duration if it's set to None
        let work_duration =
            work_duration.unwrap_or_else(|| rand::random_range(RANDOM_WORK_DURATION_RANGE.clone()));
        let lb_address = load_balancer_address.clone();
        let client = http_client.clone();

        // Send a request to the load balancer
        requests.spawn(async move {
            let json = json!({
                "duration": work_duration,
            });
            let response = client
                .post(&format!("http://{}/work", &lb_address))
                .json(&json)
                .send()
                .await
                .expect("Failed to execute request");
            assert_eq!(response.status().as_u16(), 200);
        });
    }
    // Wait for all requests to finish
    requests.join_all();
}

fn strat_copy(strat: &BalancingStrategy) -> BalancingStrategy {
    match strat {
        BalancingStrategy::Random => BalancingStrategy::Random,
        BalancingStrategy::LeastConnectionWithInternalStats => {
            BalancingStrategy::LeastConnectionWithInternalStats
        }
        BalancingStrategy::LeastConnectionWithStatsFromWorkers => {
            BalancingStrategy::LeastConnectionWithStatsFromWorkers
        }
        BalancingStrategy::RoundRobin(_) => BalancingStrategy::RoundRobin(AtomicUsize::default()),
        BalancingStrategy::ResourceBased => BalancingStrategy::ResourceBased,
        BalancingStrategy::WeightedWorkers => BalancingStrategy::WeightedWorkers,
    }
}
