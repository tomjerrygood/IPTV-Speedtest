use crate::AppState;
use axum::{
    extract::State,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::sync::Arc;

pub async fn handle_m3u8(State(state): State<Arc<AppState>>) -> Response {
    let body = {
        let guard = state.data.read().await;
        guard.m3u8.clone()
    };
    if body.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Not ready yet. Please wait for the first scan.",
        )
            .into_response();
    }
    let mut resp = body.into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.apple.mpegurl"),
    );
    resp
}

pub async fn handle_txt(State(state): State<Arc<AppState>>) -> Response {
    let body = {
        let guard = state.data.read().await;
        guard.txt.clone()
    };
    if body.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Not ready yet. Please wait for the first scan.",
        )
            .into_response();
    }
    let mut resp = body.into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    resp
}

pub async fn handle_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let (status, last_run) = {
        let guard = state.data.read().await;
        let s = if crate::task::is_running() {
            "updating"
        } else {
            "idle"
        };
        (s.to_string(), guard.last_run.clone())
    };
    Json(json!({
        "status": status,
        "last_run": last_run,
        "version": crate::config::VERSION,
    }))
}

pub async fn handle_force_retest(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if crate::task::is_running() {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"status": "busy"})),
        );
    }
    let workers = state.workers;
    let top_n = state.top_n;
    let per_channel = state.per_channel;
    let urls = state.urls.clone();
    tokio::spawn(crate::task::run_task(state, workers, top_n, per_channel, urls));
    (StatusCode::OK, Json(json!({"status": "started"})))
}
