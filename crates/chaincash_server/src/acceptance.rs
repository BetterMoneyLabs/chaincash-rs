use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chaincash_predicate::{
    context::{ContextProvider, NanoErg, NoteContext, PredicateContext},
    predicates::{Accept, Predicate},
};
use chaincash_services::ServerState;
use serde::Serialize;

use crate::api::ApiError;

#[derive(Default)]
struct AcceptanceContextProvider {
    issued_notes: HashMap<String, Vec<NoteContext>>,
    reserves: HashMap<String, NanoErg>,
}

impl AcceptanceContextProvider {
    fn from_state(state: &ServerState, note: NoteContext) -> Result<Self, ApiError> {
        let mut issued_notes: HashMap<String, Vec<NoteContext>> = HashMap::new();
        for stored_note in state.store.notes().note_contexts()? {
            let stored_note = NoteContext {
                nanoerg: stored_note.nanoerg,
                owner: stored_note.owner,
                issuer: stored_note.issuer,
                signers: stored_note.signers,
            };
            issued_notes
                .entry(stored_note.issuer.clone())
                .or_default()
                .push(stored_note);
        }

        issued_notes
            .entry(note.issuer.clone())
            .or_default()
            .push(note);

        Ok(Self {
            issued_notes,
            reserves: state.store.reserves().reserve_totals_by_owner()?,
        })
    }
}

impl ContextProvider for AcceptanceContextProvider {
    fn agent_issued_notes(&self, agent: &str) -> Vec<NoteContext> {
        self.issued_notes.get(agent).cloned().unwrap_or_default()
    }

    fn agent_reserves_nanoerg(&self, agent: &str) -> NanoErg {
        self.reserves.get(agent).copied().unwrap_or_default()
    }
}

#[derive(Serialize)]
struct CheckNoteResponse {
    accepted: bool,
}

async fn get_acceptance(State(state): State<Arc<ServerState>>) -> Result<Response, ApiError> {
    Ok(Json(&state.predicates).into_response())
}

fn predicates_accept_note(
    predicates: &[Predicate],
    note: NoteContext,
    provider: AcceptanceContextProvider,
) -> bool {
    let context = PredicateContext { note, provider };

    predicates
        .iter()
        .any(|predicate| predicate.accept(&context))
}

async fn check_note(
    State(state): State<Arc<ServerState>>,
    Json(note): Json<NoteContext>,
) -> Result<Response, ApiError> {
    let provider = AcceptanceContextProvider::from_state(&state, note.clone())?;
    let accepted = predicates_accept_note(&state.predicates, note, provider);

    Ok(Json(CheckNoteResponse { accepted }).into_response())
}

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(get_acceptance))
        .route("/checkNote", post(check_note))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use chaincash_store::{ChainCashStore, Update};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    fn test_state(predicates: Vec<Predicate>) -> Arc<ServerState> {
        let node = ergo_client::node::NodeClient::from_url_str(
            "http://127.0.0.1:9052",
            "hello".to_string(),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        let store = ChainCashStore::open_in_memory().unwrap();
        store.update().unwrap();

        Arc::new(ServerState::new(node, store, predicates))
    }

    fn note_json(owner: &str) -> Value {
        json!({
            "nanoerg": 1000,
            "owner": owner,
            "issuer": "issuer1",
            "signers": ["issuer1"]
        })
    }

    #[tokio::test]
    async fn check_note_returns_true_when_predicate_accepts() {
        let predicate = serde_json::from_value(json!({
            "type": "whitelist",
            "kind": "owner",
            "agents": ["owner1"]
        }))
        .unwrap();

        let response = router()
            .with_state(test_state(vec![predicate]))
            .oneshot(
                Request::post("/checkNote")
                    .header("content-type", "application/json")
                    .body(Body::from(note_json("owner1").to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({ "accepted": true })
        );
    }

    #[tokio::test]
    async fn check_note_returns_false_when_no_predicate_accepts() {
        let predicate = serde_json::from_value(json!({
            "type": "whitelist",
            "kind": "owner",
            "agents": ["owner1"]
        }))
        .unwrap();

        let response = router()
            .with_state(test_state(vec![predicate]))
            .oneshot(
                Request::post("/checkNote")
                    .header("content-type", "application/json")
                    .body(Body::from(note_json("owner2").to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({ "accepted": false })
        );
    }
}
