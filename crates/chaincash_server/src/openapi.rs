use std::sync::Arc;

use axum::Router;
use chaincash_offchain::boxes::ReserveBoxSpec;
use chaincash_offchain::transactions::notes::MintNoteRequest;
use chaincash_offchain::transactions::reserves::MintReserveRequest;
use chaincash_predicate::predicates::blacklist::{Blacklist, BlacklistKind};
use chaincash_predicate::predicates::collateral::{Collateral, CollateralAlgorithm};
use chaincash_predicate::predicates::or::Or;
use chaincash_predicate::predicates::whitelist::{Whitelist, WhitelistKind};
use chaincash_predicate::predicates::Predicate;
use chaincash_services::transaction::{RedeemNoteRequest, SpendNoteRequest, TopUpReserveRequest};
use chaincash_services::ServerState;
use chaincash_store::notes::{Note, NoteWithHistory, OwnershipEntry};
use serde::Serialize;
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

#[derive(Serialize, ToSchema)]
pub struct HealthcheckResponse {
    pub status: String,
    #[schema(value_type = Object)]
    pub response: serde_json::Value,
}

#[derive(Serialize, ToSchema)]
pub struct ApiErrorDetail {
    pub detail: String,
}

#[derive(Serialize, ToSchema)]
pub struct ApiErrorResponse {
    pub error: ApiErrorDetail,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TxIdResponse {
    pub tx_id: String,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MintNoteApiResponse {
    pub tx_id: String,
    pub note_id: String,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MintReserveApiResponse {
    pub tx_id: String,
    pub reserve_nft_id: String,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        crate::healthcheck::healthcheck,
        crate::acceptance::get_acceptance,
        crate::notes::list_wallet_notes,
        crate::notes::by_pubkey,
        crate::notes::mint_note,
        crate::notes::spend_note,
        crate::notes::redeem_note,
        crate::reserves::mint_reserve,
        crate::reserves::top_up_reserve,
        crate::reserves::list_wallet_reserves,
    ),
    components(
        schemas(
            HealthcheckResponse,
            ApiErrorDetail,
            ApiErrorResponse,
            TxIdResponse,
            MintNoteApiResponse,
            MintReserveApiResponse,
            MintNoteRequest,
            SpendNoteRequest,
            RedeemNoteRequest,
            Note,
            OwnershipEntry,
            NoteWithHistory,
            MintReserveRequest,
            TopUpReserveRequest,
            ReserveBoxSpec,
            Predicate,
            Or,
            Whitelist,
            WhitelistKind,
            Blacklist,
            BlacklistKind,
            Collateral,
            CollateralAlgorithm,
        )
    ),
    tags(
        (name = "healthcheck", description = "Server and node health check endpoints"),
        (name = "acceptance", description = "Note acceptance predicate endpoints"),
        (name = "notes", description = "ChainCash note management and transaction endpoints"),
        (name = "reserves", description = "ChainCash reserve management and transaction endpoints")
    )
)]
pub struct OpenApiDoc;

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", OpenApiDoc::openapi()))
}
