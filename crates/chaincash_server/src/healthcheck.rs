use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chaincash_services::ServerState;
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn healthcheck(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let url = state.node.endpoints().url();
    let url2 = url.join("blockchain/indexedHeight").map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to join URL: {}", e),
        )
    })?;
    let response = reqwest::get(url2)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to make request: {}", e),
            )
        })?
        .text()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get response text: {}", e),
            )
        })?;

    let response_json: Value = serde_json::from_str(&response).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Invalid JSON response: {}", e),
        )
    })?;
    let combined = json!({
        "status": "Node running",
        "response": response_json
    });

    Ok(Json(combined))
}
