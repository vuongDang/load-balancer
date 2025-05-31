use load_balancer::{load_balancer::LoadBalancer, tracing::init_tracing, worker::WorkerServer};
use tokio::task::JoinSet;

static NB_WORKERS: u8 = 10;
static IP: &'static str = "127.0.0.1";

#[tokio::main]
async fn main() {
    // Tracing
    init_tracing().expect("Failed to init tracing");

    // Start workers
    let mut workers = JoinSet::new();
    let mut workers_config = Vec::with_capacity(NB_WORKERS as usize);
    for id in 0..NB_WORKERS {
        let addr = format!("{}:0", IP);
        let worker = WorkerServer::build(id, &addr)
            .await
            .expect("Failed to run server");
        workers_config.push(worker.config.clone());
        workers.spawn(async move {
            worker.run().await.expect("Failed to run the app");
        });
    }

    // Start load balancer
    LoadBalancer::run(&format!("{}:3000", IP), workers_config)
        .await
        .expect("Failed to start load balander")
    // workers.join_all().await;
}
