//! ChainCash payment server creation and serving.
use std::sync::Arc;

use axum::{routing::get, Router};
use chaincash_services::ServerState;
use tracing::info;

use crate::api;
use crate::healthcheck;

pub struct Server;

impl Server {
    pub fn router() -> Router<Arc<ServerState>> {
        Router::new()
            .route("/healthcheck", get(healthcheck::healthcheck))
            .nest("/api", api::router())
    }

    /// Serves the ChainCash payment server on the given listener forever
    /// using the supplied server state.
    pub async fn serve(
        listener: std::net::TcpListener,
        state: Arc<ServerState>,
    ) -> Result<(), crate::Error> {
        info!("server started on listener: {:?}", listener);

        axum::serve::serve(
            listener.try_into()?,
            Self::router().with_state(state).into_make_service(),
        )
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use chaincash_store::ChainCashStore;
    use tower::ServiceExt;

    use super::*;

    pub fn test_server() -> Arc<ServerState> {
        // node shouldn't be actually used in unit tests
        let node = ergo_client::node::NodeClient::from_url_str(
            "http://127.0.0.1:9052",
            "hello".to_string(),
            std::time::Duration::from_secs(5),
        )
        .unwrap();

        Arc::new(ServerState::new(
            node,
            ChainCashStore::open_in_memory().unwrap(),
            vec![],
        ))
    }
    #[tokio::test]
    async fn test_healthcheck() {
        let response = Server::router()
            .with_state(test_server())
            .oneshot(Request::get("/healthcheck").body(Body::default()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
