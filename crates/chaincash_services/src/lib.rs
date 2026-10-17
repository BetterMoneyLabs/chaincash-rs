use chaincash_predicate::predicates::Predicate;
use chaincash_store::ChainCashStore;
use compiler::Compiler;
use ergo_client::node::NodeClient;
use ergo_lib::{ergo_chain_types::EcPoint, ergotree_ir::chain::address::Address};
use transaction::{TransactionService, TransactionServiceError};
use wallet::LocalWallet;

pub mod compiler;
pub mod scanner;
pub mod transaction;
pub mod wallet;

#[derive(Clone)]
pub struct ServerState {
    pub store: ChainCashStore,
    pub node: NodeClient,
    compiler: Compiler,
    pub predicates: Vec<Predicate>,
    pub wallet: Option<LocalWallet>,
}

impl ServerState {
    pub fn new(node: NodeClient, store: ChainCashStore, predicates: Vec<Predicate>) -> Self {
        Self::with_wallet(node, store, predicates, None)
    }

    pub fn with_wallet(
        node: NodeClient,
        store: ChainCashStore,
        predicates: Vec<Predicate>,
        wallet: Option<LocalWallet>,
    ) -> Self {
        ServerState {
            compiler: Compiler::new(node.clone()),
            node,
            store,
            predicates,
            wallet,
        }
    }

    pub async fn wallet_pubkeys(&self) -> Result<Vec<EcPoint>, TransactionServiceError> {
        if let Some(wallet) = &self.wallet {
            return Ok(wallet.pubkeys());
        }
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
        TransactionService::with_wallet(
            &self.node,
            &self.store,
            &self.compiler,
            self.wallet.as_ref(),
        )
    }
}
