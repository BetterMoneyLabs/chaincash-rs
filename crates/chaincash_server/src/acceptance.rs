use std::sync::Arc;

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chaincash_services::ServerState;

use crate::api::ApiError;

#[utoipa::path(
    get,
    path = "/api/v1/acceptance",
    tag = "acceptance",
    responses(
        (status = 200, description = "List of configured note acceptance predicates", body = [chaincash_predicate::predicates::Predicate]),
        (status = 500, description = "Internal server error", body = crate::openapi::ApiErrorResponse)
    )
)]
async fn get_acceptance(State(state): State<Arc<ServerState>>) -> Result<Response, ApiError> {
    Ok(Json(&state.predicates).into_response())
}

pub fn router() -> Router<Arc<ServerState>> {
    Router::new().route("/", get(get_acceptance))
}
