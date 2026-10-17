use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chaincash_offchain::transactions::reserves::{MintReserveRequest, SignedReserveResponse};
use chaincash_services::transaction::TopUpReserveRequest;
use chaincash_services::ServerState;
use serde_json::json;

use crate::api::ApiError;

#[utoipa::path(
    post,
    path = "/api/v1/reserves/mint",
    tag = "reserves",
    request_body = MintReserveRequest,
    responses(
        (status = 200, description = "Reserve minted", body = crate::openapi::MintReserveApiResponse),
        (status = 500, description = "Transaction or store error", body = crate::openapi::ApiErrorResponse)
    )
)]
async fn mint_reserve(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<MintReserveRequest>,
) -> Result<Response, ApiError> {
    let SignedReserveResponse {
        reserve_box,
        transaction,
    } = state.tx_service().mint_reserve(body).await?;
    // store the reserve now or when it hits the chain? we probably need to track the box?
    let response = Json(json!({
        "txId": transaction.id(),
        "reserveNftId": reserve_box.identifier
    }));
    Ok(response.into_response())
}

#[utoipa::path(
    post,
    path = "/api/v1/reserves/topup",
    tag = "reserves",
    request_body = TopUpReserveRequest,
    responses(
        (status = 200, description = "Reserve topped up", body = crate::openapi::TxIdResponse),
        (status = 500, description = "Transaction or store error", body = crate::openapi::ApiErrorResponse)
    )
)]
async fn top_up_reserve(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<TopUpReserveRequest>,
) -> Result<Response, ApiError> {
    let SignedReserveResponse {
        reserve_box: _,
        transaction,
    } = state.tx_service().top_up_reserve(body).await?;
    let response = Json(json!({
        "txId": transaction.id(),
    }));
    Ok(response.into_response())
}

#[utoipa::path(
    get,
    path = "/api/v1/reserves/wallet",
    tag = "reserves",
    responses(
        (status = 200, description = "List of reserves owned by wallet", body = [chaincash_offchain::boxes::ReserveBoxSpec]),
        (status = 500, description = "Transaction or store error", body = crate::openapi::ApiErrorResponse)
    )
)]
async fn list_wallet_reserves(State(state): State<Arc<ServerState>>) -> Result<Response, ApiError> {
    Ok(Json(
        state
            .store
            .reserves()
            .reserve_boxes_by_pubkeys(&state.wallet_pubkeys().await?)?,
    )
    .into_response())
}

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/mint", post(mint_reserve))
        .route("/topup", post(top_up_reserve))
        .route("/wallet", get(list_wallet_reserves))
}
