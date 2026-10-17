use std::sync::Arc;

use chaincash_offchain::node::node_from_config;
use chaincash_predicate::predicates::Predicate;
use chaincash_server::Server;
use chaincash_services::{scanner::start_scanner, ServerState};
use chaincash_store::{ChainCashStore, Update};
use config::{Environment, File};
use thiserror::Error;
use tracing::info;

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] chaincash_store::Error),

    #[error(transparent)]
    Server(#[from] chaincash_server::Error),

    #[error(transparent)]
    Node(#[from] chaincash_offchain::node::NodeError),

    #[error("Failed to load chaincash predicates specified in config file")]
    LoadPredicate(#[from] chaincash_predicate::Error),

    #[error(transparent)]
    Config(#[from] config::ConfigError),
}

#[derive(serde::Deserialize, Debug)]
pub struct ChainCashConfig {
    server: chaincash_server::Config,
    store: chaincash_store::Config,
    node: chaincash_offchain::node::Config,
    acceptance: chaincash_predicate::Config,
}

impl ChainCashConfig {
    pub fn new() -> Result<Self, Error> {
        let c = config::Config::builder()
            .add_source(File::with_name("config/default"))
            .add_source(File::with_name("config/local").required(false))
            .add_source(Environment::with_prefix("chaincash"))
            .build()?;

        Ok(c.try_deserialize()?)
    }
}

/// Facade class encompassing all components that make up the `chaincash` application.
pub struct ChainCashApp {
    config: ChainCashConfig,
}

impl ChainCashApp {
    pub fn new(config: ChainCashConfig) -> Self {
        Self { config }
    }

    pub async fn run(&self) -> Result<(), Error> {
        let store = ChainCashStore::open(&self.config.store.url)?;

        if store.has_updates()? {
            store.update()?;
        }

        let predicates = self
            .config
            .acceptance
            .predicates
            .iter()
            .map(Predicate::from_file)
            .collect::<Result<Vec<_>, _>>()?;

        info!("loaded {} predicates from files", predicates.len());

        let listener = listenfd::ListenFd::from_env()
            .take_tcp_listener(0)
            .unwrap()
            .unwrap_or_else(|| {
                std::net::TcpListener::bind(format!(
                    "{}:{}",
                    self.config.server.url, self.config.server.port
                ))
                .unwrap()
            });

        let node = node_from_config(&self.config.node)?;
        let wallet_password = self.config.node.wallet_password().map(str::to_owned);

        let state = Arc::new(ServerState::with_wallet_password(
            node,
            store,
            predicates,
            wallet_password,
        ));
        start_scanner(state.clone()).await.unwrap();
        Ok(Server::serve(listener, state).await?)
    }
}
