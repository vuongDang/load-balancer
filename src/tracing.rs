use axum::{body::Body, http::Request, response::Response};
use tracing::{Level, Span};
use tracing_error::ErrorLayer;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

pub fn init_tracing(env_lvl: &str) -> color_eyre::eyre::Result<()> {
    // Create a layer that logs to stdout
    let fmt_layer = tracing_subscriber::fmt::layer().compact();

    // Create a layer that filters logs based on the environment variable
    let filter_layer =
        EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(env_lvl))?;

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(filter_layer)
        .with(ErrorLayer::default()) // Add the error layer to capture error contexts
        .init();

    Ok(())
}

pub fn make_span_with_request_id(request: &Request<Body>) -> Span {
    let request_id = uuid::Uuid::new_v4();
    tracing::span!(
        tracing::Level::INFO,
        "[REQUEST]",
        method = display(request.method()),
        uri = display(request.uri()),
        request_id = display(request_id),
        version = debug(request.version()),
    )
}

pub fn on_request(_request: &Request<Body>, _span: &Span) {
    tracing::event!(Level::INFO, "[REQUEST_START]");
}

pub fn on_response(response: &Response, latency: std::time::Duration, _span: &Span) {
    let status = response.status();
    let status_code = status.as_u16();
    let status_code_class = status_code / 100;
    match status_code_class {
        4..=5 => {
            tracing::event!(
                tracing::Level::ERROR,
                latency = ?latency,
                status = status_code,
                "[REQUEST_END]",
            );
        }
        _ => {
            tracing::event!(
                Level::INFO,
                latency = ?latency,
                status = status_code,
                "[REQUEST_END]",
            );
        }
    }
}
