use load_balancer::{tracing::init_tracing, worker::WorkerServer};
use tokio::task::JoinSet;

static NB_WORKERS: u8 = 10;
static IP: &'static str = "127.0.0.1";

#[tokio::main]
async fn main() {
    init_tracing().expect("Failed to init tracing");
    let mut workers = JoinSet::new();
    for id in 0..NB_WORKERS {
        workers.spawn(async move {
            let port = 3000 + id as u16;
            let addr = format!("{}:{}", IP, port);
            let worker = WorkerServer::build(id, &addr)
                .await
                .expect("Failed to run server");
            worker.run().await.expect("Failed to run the app");
        });
    }
    workers.join_all().await;
}
