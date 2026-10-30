use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chaincash_offchain::transactions::reserves::{MintReserveRequest, SignedReserveResponse};
use chaincash_services::transaction::{
    InitiateRefundRequest, RefundActionResponse, RefundByReserveRequest, TopUpReserveRequest,
};
use chaincash_services::ServerState;
use chaincash_store::refunds::{RefundFilter, RefundStatus, RefundSummary};
use ergo_lib::ergotree_ir::chain::token::TokenId;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api::ApiError;

#[derive(Debug, Deserialize)]
pub struct ListRefundsQuery {
    pub reserve_id: Option<TokenId>,
    pub status: Option<RefundStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefundTxApiResponse {
    #[serde(rename = "txId")]
    pub tx_id: String,
    pub refund: RefundSummary,
}

fn build_refund_tx_response(res: RefundActionResponse) -> Result<Response, ApiError> {
    let tx_id = String::from(res.transaction.id());
    let refund = res.refund.into_summary(Some(res.current_height))?;
    Ok(Json(RefundTxApiResponse { tx_id, refund }).into_response())
}

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

async fn list_wallet_reserves(State(state): State<Arc<ServerState>>) -> Result<Response, ApiError> {
    Ok(Json(
        state
            .store
            .reserves()
            .reserve_boxes_by_pubkeys(&state.wallet_pubkeys().await?)?,
    )
    .into_response())
}

async fn initiate_refund(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<InitiateRefundRequest>,
) -> Result<Response, ApiError> {
    let res = state.tx_service().initiate_refund(body).await?;
    build_refund_tx_response(res)
}

async fn cancel_refund(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<RefundByReserveRequest>,
) -> Result<Response, ApiError> {
    let res = state.tx_service().cancel_refund(body).await?;
    build_refund_tx_response(res)
}

async fn complete_refund(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<RefundByReserveRequest>,
) -> Result<Response, ApiError> {
    let res = state.tx_service().complete_refund(&body.reserve_id).await?;
    build_refund_tx_response(res)
}

async fn list_refunds(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<ListRefundsQuery>,
) -> Result<Response, ApiError> {
    let current_height = state.current_height().await.ok();
    let records = state.store.refunds().list(RefundFilter {
        reserve_id: query.reserve_id,
        status: query.status,
    })?;
    let summaries = records
        .into_iter()
        .map(|rec| rec.into_summary(current_height))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(summaries).into_response())
}

async fn get_refund_by_id(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<i32>,
) -> Result<Response, ApiError> {
    let current_height = state.current_height().await.ok();
    let record = state
        .store
        .refunds()
        .get_by_id(id)?
        .ok_or_else(|| ApiError::NotFound(format!("Refund {id} not found")))?;
    Ok(Json(record.into_summary(current_height)?).into_response())
}

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/mint", post(mint_reserve))
        .route("/topup", post(top_up_reserve))
        .route("/wallet", get(list_wallet_reserves))
        .route("/refund", post(initiate_refund))
        .route("/refund/cancel", post(cancel_refund))
        .route("/refund/complete", post(complete_refund))
        .route("/refunds", get(list_refunds))
        .route("/refunds/:id", get(get_refund_by_id))
}

#[cfg(test)]
mod tests {
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use chaincash_offchain::boxes::ReserveBoxSpec;
    use chaincash_offchain::transactions::reserves::REFUND_TIMELOCK_BLOCKS;
    use chaincash_store::{ChainCashStore, Update};
    use ergo_lib::{
        chain::{ergo_box::box_builder::ErgoBoxCandidateBuilder, transaction::TxId},
        ergo_chain_types::Digest32,
        ergotree_interpreter::sigma_protocol::private_input::DlogProverInput,
        ergotree_ir::chain::{
            address::Address,
            ergo_box::{box_value::BoxValue, ErgoBox, NonMandatoryRegisterId},
            token::Token,
        },
    };
    use tower::ServiceExt;

    use crate::Server;

    use super::*;

    const RESERVE_A_HEX: &str = "0f44aa54140dbd5368b44358630d5ca4e38e6405f76bd987e18d7eae667915db";
    const RESERVE_B_HEX: &str = "161A3A5250655368566D597133743677397A24432646294A404D635166546A57";

    fn token_id(hex: &str) -> TokenId {
        TokenId::from(Digest32::try_from(hex.to_owned()).unwrap())
    }

    fn setup_state() -> Arc<ServerState> {
        // Closed local port so offline queries fail fast without reaching the network
        let node = ergo_client::node::NodeClient::from_url_str(
            "http://127.0.0.1:9",
            "testkey".to_string(),
            std::time::Duration::from_millis(50),
        )
        .unwrap();
        let store = ChainCashStore::open_in_memory().unwrap();
        store.update().unwrap();
        Arc::new(ServerState::new(node, store, vec![]))
    }

    fn insert_test_reserve(state: &ServerState, hex: &str, pending: Option<(i32, i64)>) {
        let sk = DlogProverInput::random();
        let pk = *sk.public_image().h.clone();
        let script = Address::P2Pk(sk.public_image()).script().unwrap();
        let mut builder =
            ErgoBoxCandidateBuilder::new(BoxValue::new(10_000_000_000).unwrap(), script, 1_000_000);
        builder.set_register_value(NonMandatoryRegisterId::R4, pk.into());
        if let Some((h, amt)) = pending {
            builder.set_register_value(NonMandatoryRegisterId::R5, h.into());
            builder.set_register_value(NonMandatoryRegisterId::R6, amt.into());
        }
        builder.add_token(Token {
            token_id: token_id(hex),
            amount: 1.try_into().unwrap(),
        });
        let ergo_box =
            ErgoBox::from_box_candidate(&builder.build().unwrap(), TxId::zero(), 0).unwrap();
        let spec = ReserveBoxSpec::try_from(&ergo_box).unwrap();
        state.store.reserves().add_or_update(&spec).unwrap();
    }

    #[tokio::test]
    async fn test_get_past_and_current_refunds_with_filters_and_by_id() {
        let state = setup_state();
        let r_a = token_id(RESERVE_A_HEX);
        let r_b = token_id(RESERVE_B_HEX);

        let past = state
            .store
            .refunds()
            .create_pending(
                &r_a,
                Some("box_old"),
                4_000_000_000,
                100_000,
                Some("tx_init_old"),
            )
            .unwrap();
        state
            .store
            .refunds()
            .mark_completed(
                past.id,
                Some("box_old"),
                4_000_000_000,
                100_000 + REFUND_TIMELOCK_BLOCKS as i32,
                "tx_settle_old",
            )
            .unwrap();

        let current_a = state
            .store
            .refunds()
            .create_pending(
                &r_a,
                Some("box_new"),
                2_000_000_000,
                200_000,
                Some("tx_init_new"),
            )
            .unwrap();

        let cancelled_b = state
            .store
            .refunds()
            .create_pending(
                &r_b,
                Some("box_b"),
                1_500_000_000,
                150_000,
                Some("tx_init_b"),
            )
            .unwrap();
        state
            .store
            .refunds()
            .mark_cancelled(
                cancelled_b.id,
                Some("box_b"),
                Some(150_100),
                Some("tx_cancel_b"),
            )
            .unwrap();

        let app = Server::router().with_state(state);

        // 1. List all refunds (newest first)
        let resp = app
            .clone()
            .oneshot(
                Request::get("/api/v1/reserves/refunds")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let all: Vec<RefundSummary> = serde_json::from_slice(&body).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id, cancelled_b.id);
        assert_eq!(all[0].status, RefundStatus::Cancelled);
        assert_eq!(all[1].id, current_a.id);
        assert_eq!(all[1].status, RefundStatus::Pending);
        assert_eq!(all[1].unlock_height, 200_000 + REFUND_TIMELOCK_BLOCKS);
        assert_eq!(all[2].id, past.id);
        assert_eq!(all[2].status, RefundStatus::Completed);
        assert_eq!(all[2].refunded_amount, Some(4_000_000_000));

        // 2. Filter by reserve_id and status
        let resp = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/reserves/refunds?reserve_id={RESERVE_A_HEX}&status=pending"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let filtered: Vec<RefundSummary> = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, current_a.id);

        // 3. Get single refund by id & 404 when missing
        let resp = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/reserves/refunds/{}", past.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let single: RefundSummary = serde_json::from_slice(&body).unwrap();
        assert_eq!(single.id, past.id);
        assert_eq!(single.refund_tx_id.as_deref(), Some("tx_settle_old"));

        let missing_resp = app
            .clone()
            .oneshot(
                Request::get("/api/v1/reserves/refunds/99999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_resp.status(), StatusCode::NOT_FOUND);

        // 4. Invalid query parameters return 400 Bad Request
        let bad_query = app
            .oneshot(
                Request::get("/api/v1/reserves/refunds?status=unknown_state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_query.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_refund_post_endpoints_not_found_and_conflict_guards() {
        let state = setup_state();
        // Reserve B has an active refund on chain
        insert_test_reserve(&state, RESERVE_B_HEX, Some((1_000_000, 2_000_000_000)));
        let app = Server::router().with_state(state);

        // Unknown reserve -> 404 Not Found
        let not_found_resp = app
            .clone()
            .oneshot(
                Request::post("/api/v1/reserves/refund")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "reserve_id": RESERVE_A_HEX,
                            "amount": 1_000_000_000_u64
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(not_found_resp.status(), StatusCode::NOT_FOUND);

        // Reserve B already has an active refund -> 409 Conflict before touching the node
        let conflict_resp = app
            .oneshot(
                Request::post("/api/v1/reserves/refund")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "reserve_id": RESERVE_B_HEX,
                            "amount": 1_000_000_000_u64
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(conflict_resp.status(), StatusCode::CONFLICT);
    }
}
