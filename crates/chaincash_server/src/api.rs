use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use chaincash_offchain::transactions::TransactionError;
use chaincash_services::transaction::TransactionServiceError;
use chaincash_services::ServerState;
use serde_json::json;
use thiserror::Error;

trait AsStatusCode {
    fn as_status_code(&self) -> StatusCode;
}

impl AsStatusCode for chaincash_offchain::node::NodeError {
    fn as_status_code(&self) -> StatusCode {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

impl AsStatusCode for TransactionError {
    fn as_status_code(&self) -> StatusCode {
        match self {
            TransactionError::RefundAlreadyActive(_) => StatusCode::CONFLICT,
            _ => StatusCode::BAD_REQUEST,
        }
    }
}

impl AsStatusCode for chaincash_store::Error {
    fn as_status_code(&self) -> StatusCode {
        match self {
            err if err.is_not_found() => StatusCode::NOT_FOUND,
            chaincash_store::Error::DuplicateActiveRefund(_) => StatusCode::CONFLICT,
            chaincash_store::Error::InvalidRefundStatus(_)
            | chaincash_store::Error::InvalidReserveTokenId(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl AsStatusCode for TransactionServiceError {
    fn as_status_code(&self) -> StatusCode {
        match self {
            TransactionServiceError::ReserveBoxNotFound
            | TransactionServiceError::NoActiveRefund(_) => StatusCode::NOT_FOUND,
            TransactionServiceError::RefundExecutionInProgress(_) => StatusCode::CONFLICT,
            TransactionServiceError::TransactionBuilding(e) => e.as_status_code(),
            TransactionServiceError::BoxValue(_) => StatusCode::BAD_REQUEST,
            TransactionServiceError::Store(e) => e.as_status_code(),
            TransactionServiceError::ChangeAddressNotSet
            | TransactionServiceError::BoxSelection(_)
            | TransactionServiceError::Node(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Transaction service error: {0}")]
    TransactionService(#[from] TransactionServiceError),
    #[error("Store error: {0}")]
    StoreError(#[from] chaincash_store::Error),
    #[error("{0}")]
    NotFound(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status_code, msg) = match self {
            ApiError::TransactionService(e) => (e.as_status_code(), e.to_string()),
            ApiError::StoreError(e) => (e.as_status_code(), e.to_string()),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
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
