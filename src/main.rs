use load_balancer::{WorkerServer, tracing::init_tracing};

#[tokio::main]
async fn main() {
    init_tracing().expect("Failed to init tracing");
    WorkerServer::run("127.0.0.1:3000".to_string())
        .await
        .expect("Failed to run server");
}
