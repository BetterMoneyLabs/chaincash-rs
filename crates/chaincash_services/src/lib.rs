use chain::ChainSource;
use chaincash_predicate::predicates::Predicate;
use chaincash_store::ChainCashStore;
use compiler::Compiler;
use ergo_client::node::{NodeClient, NodeError};
use ergo_lib::{ergo_chain_types::EcPoint, ergotree_ir::chain::address::Address};
use transaction::{TransactionService, TransactionServiceError};

pub mod chain;
pub mod compiler;
pub mod scanner;
pub mod transaction;

#[derive(Clone)]
pub struct ServerState {
    pub store: ChainCashStore,
    pub node: NodeClient,
    pub chain: ChainSource,
    compiler: Compiler,
    pub predicates: Vec<Predicate>,
}

impl ServerState {
    pub fn new(
        node: NodeClient,
        api_key: &str,
        store: ChainCashStore,
        predicates: Vec<Predicate>,
    ) -> Result<Self, NodeError> {
        Ok(ServerState {
            compiler: Compiler::new(node.clone()),
            chain: ChainSource::new(node.clone(), api_key)?,
            node,
            store,
            predicates,
        })
    }

    pub async fn wallet_pubkeys(&self) -> Result<Vec<EcPoint>, TransactionServiceError> {
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
        TransactionService::new(&self.node, &self.chain, &self.store, &self.compiler)
    }
}
