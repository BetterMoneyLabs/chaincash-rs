use chaincash_predicate::predicates::Predicate;
use chaincash_store::ChainCashStore;
use compiler::Compiler;
use ergo_client::node::{endpoints::wallet::StatusResponse, NodeClient};
use ergo_lib::{ergo_chain_types::EcPoint, ergotree_ir::chain::address::Address};
use transaction::{TransactionService, TransactionServiceError};

pub mod compiler;
pub mod scanner;
pub mod transaction;

#[derive(Clone)]
pub struct ServerState {
    pub store: ChainCashStore,
    pub node: NodeClient,
    compiler: Compiler,
    pub predicates: Vec<Predicate>,
    wallet_password: Option<String>,
}

impl ServerState {
    pub fn new(node: NodeClient, store: ChainCashStore, predicates: Vec<Predicate>) -> Self {
        Self::with_wallet_password(node, store, predicates, None)
    }

    pub fn with_wallet_password(
        node: NodeClient,
        store: ChainCashStore,
        predicates: Vec<Predicate>,
        wallet_password: Option<String>,
    ) -> Self {
        ServerState {
            compiler: Compiler::new(node.clone()),
            node,
            store,
            predicates,
            wallet_password,
        }
    }

    pub async fn ensure_wallet_unlocked(&self) -> Result<StatusResponse, TransactionServiceError> {
        self.tx_service().ensure_wallet_unlocked().await
    }

    pub async fn wallet_pubkeys(&self) -> Result<Vec<EcPoint>, TransactionServiceError> {
        self.ensure_wallet_unlocked().await?;
        Ok(self
            .node
            .endpoints()
            .wallet()?
            .get_addresses()
            .await?
            .into_iter()
            .filter_map(|addr| match addr.address() {
                Address::P2Pk(provedlog) => Some(*provedlog.h),
                _ => None,
            })
            .collect())
    }

    pub fn tx_service(&self) -> TransactionService {
        TransactionService::with_wallet_password(
            &self.node,
            &self.store,
            &self.compiler,
            self.wallet_password.as_deref(),
        )
    }
}
