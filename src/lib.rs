use ::tracing::error;
use color_eyre::Result;
use http_body_util::Full;
use hyper::{Request, Response, body::Bytes, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use tokio::net::TcpListener;

pub mod tracing;

pub struct WorkerServer {
    pub address: String,
}

impl WorkerServer {
    pub async fn run(addr: String) -> Result<()> {
        let listener = TcpListener::bind(addr).await?;
        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);
            tokio::task::spawn(async move {
                let http = http1::Builder::new();
                if let Err(err) = http.serve_connection(io, service_fn(hello)).await {
                    error!("Error serving connection {:?}", err)
                }
            });
        }
    }
}

async fn hello(_: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("Hello, I'm good"))))
}
