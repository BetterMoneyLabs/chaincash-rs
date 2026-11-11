use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chaincash_services::ServerState;
use serde_json::{json, Value};
use std::sync::Arc;

fn internal_error(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub async fn healthcheck(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let info = state
        .node
        .endpoints()
        .root()
        .map_err(internal_error)?
        .info()
        .await
        .map_err(|e| internal_error(format!("Failed to reach node: {e}")))?;

    let mut response = json!({
        "network": info.network,
        "fullHeight": info.full_height,
        "headersHeight": info.headers_height,
    });

    // Extra indexing is optional. Report its progress if the node has it enabled
    let indexed = state.chain.has_indexer().await.map_err(internal_error)?;
    if indexed {
        let indexed_height = state
            .node
            .endpoints()
            .blockchain()
            .map_err(internal_error)?
            .indexed_height()
            .await
            .map_err(internal_error)?;
        response["indexedHeight"] = indexed_height.indexed_height.into();
    }
    response["indexed"] = indexed.into();

    Ok(Json(json!({
        "status": "Node running",
        "response": response
    })))
}
