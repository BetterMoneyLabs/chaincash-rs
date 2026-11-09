use chaincash_offchain::{
    contracts::{NOTE_CONTRACT, RECEIPT_CONTRACT, RESERVE_CONTRACT},
    oracle::{buyback_nft, oracle_nft, BUYBACK_NFT, GOLD_ORACLE_NFT},
};
use ergo_client::node::NodeError;
use ergo_lib::{
    ergo_chain_types::blake2b256_hash,
    ergotree_ir::{ergo_tree::ErgoTree, serialization::SigmaSerializable},
};
use tokio::sync::OnceCell;

use crate::chain::ChainSource;

#[derive(Clone)]
pub struct Compiler {
    chain: ChainSource,
    reserve_contract: OnceCell<ErgoTree>,
    note_contract: OnceCell<ErgoTree>,
    receipt_contract: OnceCell<ErgoTree>,
}

impl Compiler {
    pub fn new(chain: ChainSource) -> Self {
        Compiler {
            chain,
            reserve_contract: OnceCell::new(),
            note_contract: OnceCell::new(),
            receipt_contract: OnceCell::new(),
        }
    }
    pub async fn reserve_contract(&self) -> Result<&ErgoTree, NodeError> {
        self.reserve_contract
            .get_or_try_init(|| async {
                let is_mainnet =
                    self.chain.node().endpoints().root()?.info().await?.network == "mainnet";
                let reserve_contract = RESERVE_CONTRACT
                    .replace(GOLD_ORACLE_NFT, &String::from(oracle_nft(is_mainnet)))
                    .replace(BUYBACK_NFT, &String::from(buyback_nft(is_mainnet)));
                if is_mainnet {
                    debug_assert_eq!(reserve_contract, RESERVE_CONTRACT);
                }
                self.chain.compile_contract(&reserve_contract).await
            })
            .await
    }
    pub async fn receipt_contract(&self) -> Result<&ErgoTree, NodeError> {
        self.receipt_contract
            .get_or_try_init(|| async {
                let reserve_tree_bytes = self
                    .reserve_contract()
                    .await?
                    .sigma_serialize_bytes()
                    .unwrap();
                let reserve_hash =
                    bs58::encode(blake2b256_hash(&reserve_tree_bytes[1..])).into_string();
                let receipt_contract =
                    RECEIPT_CONTRACT.replace("$reserveContractHash", &reserve_hash);
                self.chain.compile_contract(&receipt_contract).await
            })
            .await
    }
    pub async fn note_contract(&self) -> Result<&ErgoTree, NodeError> {
        self.note_contract
            .get_or_try_init(|| async {
                let reserve_tree_bytes = self
                    .reserve_contract()
                    .await?
                    .sigma_serialize_bytes()
                    .unwrap();
                let reserve_hash =
                    bs58::encode(blake2b256_hash(&reserve_tree_bytes[1..])).into_string();
                let receipt_tree_bytes = self
                    .receipt_contract()
                    .await?
                    .sigma_serialize_bytes()
                    .unwrap();
                let receipt_hash =
                    bs58::encode(blake2b256_hash(&receipt_tree_bytes[1..])).into_string();
                let note_contract = NOTE_CONTRACT
                    .replace("$reserveContractHash", &reserve_hash)
                    .replace("$receiptContractHash", &receipt_hash);
                self.chain.compile_contract(&note_contract).await
            })
            .await
    }
}
