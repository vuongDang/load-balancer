use load_balancer::{tracing::init_tracing, worker::WorkerServer};

#[tokio::main]
async fn main() {
    init_tracing().expect("Failed to init tracing");
    let worker = WorkerServer::build("127.0.0.1:3000")
        .await
        .expect("Failed to run server");
    worker.run().await.expect("Failed to run the app");
}
