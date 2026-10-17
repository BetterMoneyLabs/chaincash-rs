use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chaincash_offchain::transactions::notes::{
    MintNoteRequest, SignedMintNoteResponse, SignedSpendNoteResponse,
};
use chaincash_services::transaction::{RedeemNoteRequest, SpendNoteRequest};
use chaincash_services::ServerState;
use ergo_lib::ergo_chain_types::EcPoint;
use serde_json::json;

use crate::api::ApiError;

#[utoipa::path(
    post,
    path = "/api/v1/notes/mint",
    tag = "notes",
    request_body = MintNoteRequest,
    responses(
        (status = 200, description = "Note minted", body = crate::openapi::MintNoteApiResponse),
        (status = 500, description = "Transaction or store error", body = crate::openapi::ApiErrorResponse)
    )
)]
async fn mint_note(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<MintNoteRequest>,
) -> Result<Response, ApiError> {
    let SignedMintNoteResponse { note, transaction } = state.tx_service().mint_note(body).await?;
    let response = Json(json!({
        "txId": transaction.id().to_string(),
        "noteId": note.note_id,
    }));
    Ok(response.into_response())
}

#[utoipa::path(
    post,
    path = "/api/v1/notes/spend",
    tag = "notes",
    request_body = SpendNoteRequest,
    responses(
        (status = 200, description = "Note spent", body = crate::openapi::TxIdResponse),
        (status = 500, description = "Transaction or store error", body = crate::openapi::ApiErrorResponse)
    )
)]
async fn spend_note(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<SpendNoteRequest>,
) -> Result<Response, ApiError> {
    let SignedSpendNoteResponse {
        transaction,
        recipient_note: _,
        change_note: _,
    } = state.tx_service().spend_note(body).await?;
    let response = Json(json!({
        "txId": transaction.id().to_string(),
    }));
    Ok(response.into_response())
}

#[utoipa::path(
    post,
    path = "/api/v1/notes/redeem",
    tag = "notes",
    request_body = RedeemNoteRequest,
    responses(
        (status = 200, description = "Note redeemed", body = crate::openapi::TxIdResponse),
        (status = 500, description = "Transaction or store error", body = crate::openapi::ApiErrorResponse)
    )
)]
async fn redeem_note(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<RedeemNoteRequest>,
) -> Result<Response, ApiError> {
    let transaction = state.tx_service().redeem_note(body).await?;
    let response = Json(json!({
        "txId": transaction.id().to_string(),
    }));
    Ok(response.into_response())
}

#[utoipa::path(
    get,
    path = "/api/v1/notes/wallet",
    tag = "notes",
    responses(
        (status = 200, description = "List of notes owned by wallet", body = [chaincash_store::notes::NoteWithHistory]),
        (status = 500, description = "Transaction or store error", body = crate::openapi::ApiErrorResponse)
    )
)]
async fn list_wallet_notes(State(state): State<Arc<ServerState>>) -> Result<Response, ApiError> {
    let pubkeys = state.wallet_pubkeys().await?;
    let notes = state.store.notes().notes_by_pubkeys(&pubkeys)?;
    let response = Json(notes);
    Ok(response.into_response())
}

#[utoipa::path(
    get,
    path = "/api/v1/notes/byPubkey/{pubkey}",
    tag = "notes",
    params(
        ("pubkey" = String, Path, description = "Hex-encoded public key (EcPoint)")
    ),
    responses(
        (status = 200, description = "List of notes owned by public key", body = [chaincash_store::notes::NoteWithHistory]),
        (status = 500, description = "Store error", body = crate::openapi::ApiErrorResponse)
    )
)]
async fn by_pubkey(
    State(state): State<Arc<ServerState>>,
    Path(pubkey): Path<EcPoint>,
) -> Result<Response, ApiError> {
    Ok(Json(state.store.notes().notes_by_pubkeys(&[pubkey])?).into_response())
}

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/wallet", get(list_wallet_notes))
        .route("/byPubkey/:pubkey", get(by_pubkey))
        .route("/spend", post(spend_note))
        .route("/redeem", post(redeem_note))
        .route("/mint", post(mint_note))
}
