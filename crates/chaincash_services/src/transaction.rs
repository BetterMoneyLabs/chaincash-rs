use chaincash_offchain::oracle::{buyback_nft, oracle_nft};
use chaincash_offchain::transactions::notes::{
    mint_note_transaction, redeem_note, spend_note_transaction, MintNoteRequest, MintNoteResponse,
    SignedMintNoteResponse, SignedSpendNoteResponse, SpendNoteResponse,
};
use chaincash_offchain::transactions::reserves::{
    mint_reserve_transaction, top_up_reserve_transaction, MintReserveRequest, ReserveResponse,
    SignedReserveResponse,
};
use chaincash_offchain::transactions::{TransactionError, TxContext};
use chaincash_store::ChainCashStore;
use ergo_client::node::endpoints::blockchain::IndexQuery;
use ergo_client::node::NodeClient;
use ergo_lib::chain::transaction::unsigned::UnsignedTransaction;
use ergo_lib::chain::transaction::Transaction;
use ergo_lib::ergo_chain_types::EcPoint;
use ergo_lib::ergotree_interpreter::sigma_protocol::wscalar::Wscalar;
use ergo_lib::ergotree_ir::chain::address::NetworkPrefix;
use ergo_lib::ergotree_ir::chain::ergo_box::box_value::BoxValue;
use ergo_lib::ergotree_ir::chain::ergo_box::{box_value::BoxValueError, ErgoBox};
use ergo_lib::ergotree_ir::chain::token::{TokenAmount, TokenId};
use ergo_lib::wallet::box_selector::{
    BoxSelection, BoxSelector, BoxSelectorError, SimpleBoxSelector,
};
use ergo_lib::wallet::tx_builder::SUGGESTED_TX_FEE;
use serde::Deserialize;
use thiserror::Error;

use crate::compiler::Compiler;
use crate::wallet::{LocalWallet, LocalWalletError};

#[derive(Debug, Error)]
pub enum TransactionServiceError {
    #[error("Change address not set in wallet")]
    ChangeAddressNotSet,

    #[error("An error occurred while building transaction: {0}")]
    TransactionBuilding(#[from] TransactionError),

    #[error("Failed to convert ergo boxes into 'selected' boxes for transaction: {0}")]
    BoxSelection(#[from] BoxSelectorError),

    #[error("Invalid box value supplied")]
    BoxValue(#[from] BoxValueError),

    #[error("Node operation failed: {0}")]
    Node(#[from] ergo_client::node::NodeError),

    #[error("Local wallet operation failed: {0}")]
    Wallet(#[from] LocalWalletError),

    #[error("Store error: {0}")]
    Store(#[from] chaincash_store::Error),

    #[error("Reserve Box not found")]
    ReserveBoxNotFound,
}

#[derive(Deserialize)]
pub struct SpendNoteRequest {
    /// ID of note in database
    note_id: i32,
    reserve_id: TokenId,
    recipient_pubkey: EcPoint,
    amount: TokenAmount,
}

#[derive(Deserialize)]
pub struct TopUpReserveRequest {
    /// ID of note in database
    reserve_id: TokenId,
    top_up_amount: u64,
}

#[derive(Deserialize)]
pub struct RedeemNoteRequest {
    note_id: i32,
    reserve_id: TokenId,
}

#[derive(Clone)]
pub struct TransactionService<'a> {
    node: &'a NodeClient,
    compiler: &'a Compiler,
    store: &'a ChainCashStore,
    wallet: Option<&'a LocalWallet>,
}

impl<'a> TransactionService<'a> {
    pub fn new(node: &'a NodeClient, store: &'a ChainCashStore, compiler: &'a Compiler) -> Self {
        Self::with_wallet(node, store, compiler, None)
    }

    pub fn with_wallet(
        node: &'a NodeClient,
        store: &'a ChainCashStore,
        compiler: &'a Compiler,
        wallet: Option<&'a LocalWallet>,
    ) -> Self {
        Self {
            node,
            store,
            compiler,
            wallet,
        }
    }

    async fn get_utxos(&self) -> Result<Vec<ErgoBox>, TransactionServiceError> {
        if let Some(wallet) = self.wallet {
            Ok(wallet.get_utxos(self.node).await?)
        } else {
            Ok(self.node.extensions().get_utxos().await?)
        }
    }

    async fn box_selection_with_amount(
        &self,
        amount: u64,
    ) -> Result<BoxSelection<ErgoBox>, TransactionServiceError> {
        let inputs = if let Some(wallet) = self.wallet {
            wallet.get_utxos_summing_amount(self.node, amount).await?
        } else {
            self.node
                .extensions()
                .get_utxos_summing_amount(amount)
                .await?
        };
        // kinda irrelevant since we already have suitable boxes but box selectors required by ergo-lib txbuilder
        Ok(SimpleBoxSelector::new()
            .select(
                inputs,
                amount.try_into().map_err(TransactionServiceError::from)?,
                &[],
            )
            .map_err(TransactionServiceError::from)?)
    }

    async fn get_tx_ctx(&self) -> Result<TxContext, TransactionServiceError> {
        let info = self.node.endpoints().root()?.info().await?;

        if let Some(wallet) = self.wallet {
            let prefix = if info.network == "mainnet" {
                NetworkPrefix::Mainnet
            } else {
                NetworkPrefix::Testnet
            };
            return Ok(TxContext {
                current_height: info.full_height as u32,
                change_address: wallet.change_address(prefix),
                fee: *SUGGESTED_TX_FEE().as_u64(),
            });
        }

        let wallet_status = self.node.endpoints().wallet()?.status().await?;

        if wallet_status.change_address.is_empty() {
            Err(TransactionServiceError::ChangeAddressNotSet)?
        } else {
            Ok(TxContext {
                current_height: info.full_height as u32,
                change_address: wallet_status.change_address,
                fee: *SUGGESTED_TX_FEE().as_u64(),
            })
        }
    }

    async fn get_private_key(&self, owner: EcPoint) -> Result<Wscalar, TransactionServiceError> {
        if let Some(wallet) = self.wallet {
            Ok(wallet.get_private_key(&owner)?.w)
        } else {
            Ok(self.node.extensions().get_private_key(owner).await?.w)
        }
    }

    async fn sign_and_submit(
        &self,
        transaction: UnsignedTransaction,
        boxes_to_spend: Vec<ErgoBox>,
        data_boxes: Vec<ErgoBox>,
    ) -> Result<Transaction, TransactionServiceError> {
        if let Some(wallet) = self.wallet {
            Ok(wallet
                .sign_and_submit(self.node, transaction, boxes_to_spend, data_boxes)
                .await?)
        } else {
            Ok(self.node.extensions().sign_and_submit(transaction).await?)
        }
    }

    /// Create a mint reserve transaction and add minted reserve box to DB
    pub async fn mint_reserve(
        &self,
        request: MintReserveRequest,
    ) -> Result<SignedReserveResponse, TransactionServiceError> {
        let ctx = self.get_tx_ctx().await?;
        let selected_inputs = self
            .box_selection_with_amount(request.amount + ctx.fee)
            .await?;
        let boxes_to_spend: Vec<ErgoBox> = selected_inputs.boxes.as_vec().clone();
        let reserve_tree = self.compiler.reserve_contract().await?.clone();
        let ReserveResponse {
            reserve_box,
            transaction,
        } = mint_reserve_transaction(request, reserve_tree, selected_inputs, ctx)?;
        let submitted_tx = self
            .sign_and_submit(transaction, boxes_to_spend, vec![])
            .await?;
        self.store.reserves().add_or_update(&reserve_box)?;
        Ok(SignedReserveResponse {
            reserve_box,
            transaction: submitted_tx,
        })
    }

    pub async fn top_up_reserve(
        &self,
        request: TopUpReserveRequest,
    ) -> Result<SignedReserveResponse, TransactionServiceError> {
        let ctx = self.get_tx_ctx().await?;
        let wallet_boxes = self.get_utxos().await?;
        let reserve = self
            .store
            .reserves()
            .get_reserve_by_identifier(&request.reserve_id)?;
        let mut boxes_to_spend = wallet_boxes.clone();
        boxes_to_spend.push(reserve.ergo_box().clone());
        let ReserveResponse {
            reserve_box,
            transaction,
        } = top_up_reserve_transaction(&reserve, wallet_boxes, request.top_up_amount, &ctx)?;
        let submitted_tx = self
            .sign_and_submit(transaction, boxes_to_spend, vec![])
            .await?;
        self.store.reserves().add_or_update(&reserve_box)?;
        Ok(SignedReserveResponse {
            reserve_box,
            transaction: submitted_tx,
        })
    }

    pub async fn mint_note(
        &self,
        request: MintNoteRequest,
    ) -> Result<SignedMintNoteResponse, TransactionServiceError> {
        let ctx = self.get_tx_ctx().await?;
        let selected_inputs = self
            .box_selection_with_amount(BoxValue::SAFE_USER_MIN.as_u64() + ctx.fee)
            .await?;
        let boxes_to_spend: Vec<ErgoBox> = selected_inputs.boxes.as_vec().clone();
        let note_tree = self.compiler.note_contract().await?.clone();
        let MintNoteResponse { note, transaction } =
            mint_note_transaction(request, note_tree, selected_inputs, ctx)?;
        let submitted_tx = self
            .sign_and_submit(transaction, boxes_to_spend, vec![])
            .await?;
        self.store.notes().add_note(&note)?;
        Ok(SignedMintNoteResponse {
            note,
            transaction: submitted_tx,
        })
    }

    pub async fn spend_note(
        &self,
        request: SpendNoteRequest,
    ) -> Result<SignedSpendNoteResponse, TransactionServiceError> {
        let note = self.store.notes().get_note_box(request.note_id)?;
        let reserve = self
            .store
            .reserves()
            .get_reserve_by_identifier(&request.reserve_id)?;
        let private_key = self.get_private_key(note.owner.clone()).await?;
        let wallet_boxes = self.get_utxos().await?;
        let mut boxes_to_spend = wallet_boxes.clone();
        boxes_to_spend.push(note.ergo_box().clone());
        let tx_context = self.get_tx_ctx().await?;
        let SpendNoteResponse {
            transaction,
            recipient_note,
            change_note,
        } = spend_note_transaction(
            &note,
            &reserve,
            private_key,
            request.recipient_pubkey,
            *request.amount.as_u64(),
            wallet_boxes,
            &tx_context,
        )?;

        let transaction = self
            .sign_and_submit(
                transaction,
                boxes_to_spend,
                vec![reserve.ergo_box().clone()],
            )
            .await?;
        self.store.notes().delete_note(request.note_id)?;
        if let Some(ref change_note) = change_note {
            self.store.notes().add_note(change_note)?;
        }

        Ok(SignedSpendNoteResponse {
            transaction,
            recipient_note,
            change_note,
        })
    }

    pub async fn redeem_note(
        &self,
        request: RedeemNoteRequest,
    ) -> Result<Transaction, TransactionServiceError> {
        let note_box = self.store.notes().get_note_box(request.note_id)?;
        let reserve_box = self
            .store
            .reserves()
            .get_reserve_by_identifier(&request.reserve_id)?;
        let receipt_contract = self.compiler.receipt_contract().await?;
        let is_mainnet = self.node.endpoints().root()?.info().await?.network == "mainnet";
        let buyback_box = &self
            .node
            .endpoints()
            .blockchain()?
            .get_unspent_boxes_by_token_id(
                &String::from(buyback_nft(is_mainnet)),
                IndexQuery {
                    offset: 0,
                    limit: 1,
                    sort_direction:
                        ergo_client::node::endpoints::blockchain::SortDirection::Descending,
                    include_unconfirmed: true,
                },
            )
            .await?[0]
            .ergo_box;
        let wallet_boxes = self.get_utxos().await?;
        let mut boxes_to_spend = wallet_boxes.clone();
        boxes_to_spend.extend_from_slice(&[
            note_box.ergo_box().clone(),
            buyback_box.clone(),
            reserve_box.ergo_box().clone(),
        ]);
        let tx_context = self.get_tx_ctx().await?;
        let oracle_box = &self
            .node
            .endpoints()
            .blockchain()?
            .get_unspent_boxes_by_token_id(
                &String::from(oracle_nft(is_mainnet)),
                IndexQuery {
                    offset: 0,
                    limit: 1,
                    sort_direction:
                        ergo_client::node::endpoints::blockchain::SortDirection::Descending,
                    include_unconfirmed: true,
                },
            )
            .await?[0]
            .ergo_box;
        let tx = redeem_note(
            &note_box,
            &reserve_box,
            &oracle_box,
            &buyback_box,
            receipt_contract,
            wallet_boxes,
            &tx_context,
        )?;
        let tx = self
            .sign_and_submit(tx, boxes_to_spend, vec![oracle_box.clone()])
            .await?;
        Ok(tx)
    }
}
