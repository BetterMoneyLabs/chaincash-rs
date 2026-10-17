use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use chaincash_services::ServerState;
use serde_json::json;
use thiserror::Error;

#[allow(dead_code)]
trait AsStatusCode {
    fn as_status_code(&self) -> StatusCode;
}

impl AsStatusCode for chaincash_offchain::node::NodeError {
    fn as_status_code(&self) -> StatusCode {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

impl AsStatusCode for chaincash_offchain::transactions::TransactionError {
    fn as_status_code(&self) -> StatusCode {
        StatusCode::BAD_REQUEST
    }
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Transaction service error")]
    TransactionService(#[from] chaincash_services::transaction::TransactionServiceError),
    #[error("Store error: {0}")]
    StoreError(#[from] chaincash_store::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status_code, msg) = match self {
            ApiError::TransactionService(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            ApiError::StoreError(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        let body = Json(json!({
            "error": {
                "detail": msg,
            }
        }));

        (status_code, body).into_response()
    }
}

pub fn router() -> Router<Arc<ServerState>> {
    let router_v1 = Router::new()
        .nest("/reserves", crate::reserves::router())
        .nest("/notes", crate::notes::router())
        .nest("/acceptance", crate::acceptance::router());

    Router::new().nest("/v1", router_v1)
}
