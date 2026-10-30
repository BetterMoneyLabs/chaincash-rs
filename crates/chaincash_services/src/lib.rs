use chaincash_predicate::predicates::Predicate;
use chaincash_store::ChainCashStore;
use compiler::Compiler;
use ergo_client::node::NodeClient;
use ergo_lib::{ergo_chain_types::EcPoint, ergotree_ir::chain::address::Address};
use transaction::{TransactionService, TransactionServiceError};

pub mod compiler;
pub mod refund_worker;
pub mod scanner;
pub mod transaction;

#[derive(Clone)]
pub struct ServerState {
    pub store: ChainCashStore,
    pub node: NodeClient,
    compiler: Compiler,
    pub predicates: Vec<Predicate>,
}

impl ServerState {
    pub fn new(node: NodeClient, store: ChainCashStore, predicates: Vec<Predicate>) -> Self {
        ServerState {
            compiler: Compiler::new(node.clone()),
            node,
            store,
            predicates,
        }
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

    pub async fn current_height(&self) -> Result<u32, TransactionServiceError> {
        Ok(self.node.endpoints().root()?.info().await?.full_height as u32)
    }

    pub fn tx_service(&self) -> TransactionService<'_> {
        TransactionService::new(&self.node, &self.store, &self.compiler)
    }
}
