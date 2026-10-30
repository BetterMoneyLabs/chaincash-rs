use chaincash_offchain::boxes::ReserveBoxSpec;
use chaincash_offchain::oracle::{buyback_nft, oracle_nft};
use chaincash_offchain::transactions::notes::{
    mint_note_transaction, redeem_note, spend_note_transaction, MintNoteRequest, MintNoteResponse,
    SignedMintNoteResponse, SignedSpendNoteResponse, SpendNoteResponse,
};
use chaincash_offchain::transactions::reserves::{
    cancel_refund_transaction, complete_refund_transaction, initiate_refund_transaction,
    mint_reserve_transaction, top_up_reserve_transaction, MintReserveRequest, ReserveResponse,
    SignedReserveResponse,
};
use chaincash_offchain::transactions::{TransactionError, TxContext};
use chaincash_store::refunds::{RefundRecord, RefundStatus};
use chaincash_store::ChainCashStore;
use ergo_client::node::endpoints::blockchain::IndexQuery;
use ergo_client::node::NodeClient;
use ergo_lib::chain::transaction::Transaction;
use ergo_lib::ergo_chain_types::EcPoint;
use ergo_lib::ergotree_ir::chain::ergo_box::box_value::BoxValue;
use ergo_lib::ergotree_ir::chain::ergo_box::{box_value::BoxValueError, ErgoBox};
use ergo_lib::ergotree_ir::chain::token::{TokenAmount, TokenId};
use ergo_lib::wallet::box_selector::{
    BoxSelection, BoxSelector, BoxSelectorError, SimpleBoxSelector,
};
use ergo_lib::wallet::tx_builder::SUGGESTED_TX_FEE;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::compiler::Compiler;

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

    #[error("Store error: {0}")]
    Store(#[from] chaincash_store::Error),

    #[error("Reserve Box not found")]
    ReserveBoxNotFound,

    #[error("No active refund found for reserve {0}")]
    NoActiveRefund(String),

    #[error("Refund operation is already in progress for reserve {0}")]
    RefundExecutionInProgress(String),
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InitiateRefundRequest {
    /// Singleton NFT ID of the reserve to refund from
    pub reserve_id: TokenId,
    /// Optional nanoERG cap to withdraw after timelock expiry; defaults to max refundable balance
    #[serde(default)]
    pub amount: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RefundByReserveRequest {
    /// Singleton NFT ID of the reserve whose active refund is targeted
    pub reserve_id: TokenId,
}

pub struct RefundActionResponse {
    pub transaction: Transaction,
    pub refund: RefundRecord,
    pub current_height: u32,
}

#[derive(Clone)]
pub struct TransactionService<'a> {
    node: &'a NodeClient,
    compiler: &'a Compiler,
    store: &'a ChainCashStore,
}

impl<'a> TransactionService<'a> {
    pub fn new(node: &'a NodeClient, store: &'a ChainCashStore, compiler: &'a Compiler) -> Self {
        Self {
            node,
            store,
            compiler,
        }
    }

    fn load_reserve(
        &self,
        reserve_id: &TokenId,
    ) -> Result<ReserveBoxSpec, TransactionServiceError> {
        match self.store.reserves().get_reserve_by_identifier(reserve_id) {
            Ok(reserve) => Ok(reserve),
            Err(err) if err.is_not_found() => Err(TransactionServiceError::ReserveBoxNotFound),
            Err(err) => Err(TransactionServiceError::Store(err)),
        }
    }

    fn ensure_active_refund_record(
        &self,
        reserve: &ReserveBoxSpec,
    ) -> Result<RefundRecord, TransactionServiceError> {
        if let Some(existing) = self
            .store
            .refunds()
            .active_for_reserve(&reserve.identifier)?
        {
            return Ok(existing);
        }
        let Some(init_height) = reserve.refund_height else {
            return Err(TransactionServiceError::NoActiveRefund(String::from(
                reserve.identifier,
            )));
        };
        let requested_amount = reserve.refund_amount.unwrap_or(0);
        let box_id_str = reserve.box_id().to_string();
        Ok(self.store.refunds().create_pending(
            &reserve.identifier,
            Some(&box_id_str),
            requested_amount,
            init_height,
            None,
        )?)
    }

    async fn box_selection_with_amount(
        &self,
        amount: u64,
    ) -> Result<BoxSelection<ErgoBox>, TransactionServiceError> {
        let inputs = self
            .node
            .extensions()
            .get_utxos_summing_amount(amount)
            .await?;
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
        let wallet_status = self.node.endpoints().wallet()?.status().await?;
        let info = self.node.endpoints().root()?.info().await?;

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

    /// Create a mint reserve transaction and add minted reserve box to DB
    pub async fn mint_reserve(
        &self,
        request: MintReserveRequest,
    ) -> Result<SignedReserveResponse, TransactionServiceError> {
        let ctx = self.get_tx_ctx().await?;
        let selected_inputs = self
            .box_selection_with_amount(request.amount + ctx.fee)
            .await?;
        let reserve_tree = self.compiler.reserve_contract().await?.clone();
        let ReserveResponse {
            reserve_box,
            transaction,
        } = mint_reserve_transaction(request, reserve_tree, selected_inputs, ctx)?;
        let submitted_tx = self.node.extensions().sign_and_submit(transaction).await?;
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
        let wallet_boxes = self.node.extensions().get_utxos().await?;
        let reserve = self.load_reserve(&request.reserve_id)?;
        let ReserveResponse {
            reserve_box,
            transaction,
        } = top_up_reserve_transaction(&reserve, wallet_boxes, request.top_up_amount, &ctx)?;
        let submitted_tx = self.node.extensions().sign_and_submit(transaction).await?;
        self.store.reserves().add_or_update(&reserve_box)?;
        Ok(SignedReserveResponse {
            reserve_box,
            transaction: submitted_tx,
        })
    }

    /// Initiates a timelocked refund on a reserve (`action == 2`) and persists a `pending` record.
    pub async fn initiate_refund(
        &self,
        request: InitiateRefundRequest,
    ) -> Result<RefundActionResponse, TransactionServiceError> {
        let reserve = self.load_reserve(&request.reserve_id)?;
        if reserve.has_active_refund()
            || self
                .store
                .refunds()
                .active_for_reserve(&request.reserve_id)?
                .is_some()
        {
            return Err(TransactionServiceError::TransactionBuilding(
                TransactionError::RefundAlreadyActive(request.reserve_id),
            ));
        }

        let ctx = self.get_tx_ctx().await?;
        let wallet_boxes = self.node.extensions().get_utxos().await?;
        let ReserveResponse {
            reserve_box,
            transaction,
        } = initiate_refund_transaction(&reserve, wallet_boxes, request.amount, &ctx)?;

        let init_height = reserve_box
            .refund_height
            .unwrap_or(ctx.current_height as i32);
        let requested_amount = reserve_box.refund_amount.unwrap_or(0);
        let new_box_id = reserve_box.box_id().to_string();

        let submitted_tx = self.node.extensions().sign_and_submit(transaction).await?;
        let tx_id_str = String::from(submitted_tx.id());

        self.store.reserves().add_or_update(&reserve_box)?;
        let refund = self.store.refunds().create_pending(
            &request.reserve_id,
            Some(&new_box_id),
            requested_amount,
            init_height,
            Some(&tx_id_str),
        )?;

        Ok(RefundActionResponse {
            transaction: submitted_tx,
            refund,
            current_height: ctx.current_height,
        })
    }

    /// Cancels an active pending refund on a reserve (`action == 3`) and marks it `cancelled`.
    pub async fn cancel_refund(
        &self,
        request: RefundByReserveRequest,
    ) -> Result<RefundActionResponse, TransactionServiceError> {
        let reserve = self.load_reserve(&request.reserve_id)?;
        let active = self.ensure_active_refund_record(&reserve)?;
        if active.status()? == RefundStatus::Executing {
            return Err(TransactionServiceError::RefundExecutionInProgress(
                String::from(request.reserve_id),
            ));
        }
        let Some(claimed) = self.store.refunds().try_begin_execution(active.id)? else {
            return Err(TransactionServiceError::RefundExecutionInProgress(
                String::from(request.reserve_id),
            ));
        };

        let result = async {
            let ctx = self.get_tx_ctx().await?;
            let wallet_boxes = self.node.extensions().get_utxos().await?;
            let ReserveResponse {
                reserve_box,
                transaction,
            } = cancel_refund_transaction(&reserve, wallet_boxes, &ctx)?;
            let submitted_tx = self.node.extensions().sign_and_submit(transaction).await?;
            Ok::<_, TransactionServiceError>((ctx, reserve_box, submitted_tx))
        }
        .await;

        match result {
            Ok((ctx, reserve_box, submitted_tx)) => {
                let tx_id_str = String::from(submitted_tx.id());
                let spent_box_id = reserve.box_id().to_string();
                self.store.reserves().add_or_update(&reserve_box)?;
                let refund = self.store.refunds().mark_cancelled(
                    claimed.id,
                    Some(&spent_box_id),
                    i32::try_from(ctx.current_height).ok(),
                    Some(&tx_id_str),
                )?;
                Ok(RefundActionResponse {
                    transaction: submitted_tx,
                    refund,
                    current_height: ctx.current_height,
                })
            }
            Err(err) => {
                let _ = self
                    .store
                    .refunds()
                    .revert_execution_to_pending(claimed.id, claimed.error_message.as_deref());
                Err(err)
            }
        }
    }

    /// Completes an unlocked refund on `reserve_id` (`action == 4`), withdrawing up to the
    /// announced `R6` cap and transitioning the stored record to `completed`.
    pub async fn complete_refund(
        &self,
        reserve_id: &TokenId,
    ) -> Result<RefundActionResponse, TransactionServiceError> {
        let reserve = self.load_reserve(reserve_id)?;
        let active = self.ensure_active_refund_record(&reserve)?;
        if active.status()? == RefundStatus::Executing {
            return Err(TransactionServiceError::RefundExecutionInProgress(
                String::from(*reserve_id),
            ));
        }
        let Some(claimed) = self.store.refunds().try_begin_execution(active.id)? else {
            return Err(TransactionServiceError::RefundExecutionInProgress(
                String::from(*reserve_id),
            ));
        };

        let result = async {
            let ctx = self.get_tx_ctx().await?;
            let wallet_boxes = self.node.extensions().get_utxos().await?;
            let completed = complete_refund_transaction(&reserve, wallet_boxes, &ctx)?;
            let submitted_tx = self
                .node
                .extensions()
                .sign_and_submit(completed.transaction)
                .await?;
            Ok::<_, TransactionServiceError>((
                ctx,
                completed.reserve_box,
                completed.refunded_amount,
                submitted_tx,
            ))
        }
        .await;

        match result {
            Ok((ctx, new_reserve_box, refunded_amount, submitted_tx)) => {
                let tx_id_str = String::from(submitted_tx.id());
                let spent_box_id = reserve.box_id().to_string();
                self.store.reserves().add_or_update(&new_reserve_box)?;
                let refund = self.store.refunds().mark_completed(
                    claimed.id,
                    Some(&spent_box_id),
                    refunded_amount as i64,
                    ctx.current_height as i32,
                    &tx_id_str,
                )?;
                Ok(RefundActionResponse {
                    transaction: submitted_tx,
                    refund,
                    current_height: ctx.current_height,
                })
            }
            Err(err) => {
                match &err {
                    TransactionServiceError::TransactionBuilding(
                        TransactionError::RefundTimelockActive { .. },
                    ) => {
                        let _ = self.store.refunds().revert_execution_to_pending(
                            claimed.id,
                            claimed.error_message.as_deref(),
                        );
                    }
                    TransactionServiceError::TransactionBuilding(
                        TransactionError::InvalidRefundAmount { .. },
                    ) => {
                        let spent_box_id = reserve.box_id().to_string();
                        let reason = err.to_string();
                        let _ = self.store.refunds().mark_failed(
                            claimed.id,
                            Some(&spent_box_id),
                            None,
                            &reason,
                        );
                    }
                    _ => {
                        let msg = err.to_string();
                        let _ = self
                            .store
                            .refunds()
                            .revert_execution_to_pending(claimed.id, Some(&msg));
                    }
                }
                Err(err)
            }
        }
    }

    pub async fn mint_note(
        &self,
        request: MintNoteRequest,
    ) -> Result<SignedMintNoteResponse, TransactionServiceError> {
        let ctx = self.get_tx_ctx().await?;
        let selected_inputs = self
            .box_selection_with_amount(BoxValue::SAFE_USER_MIN.as_u64() + ctx.fee)
            .await?;
        let note_tree = self.compiler.note_contract().await?.clone();
        let MintNoteResponse { note, transaction } =
            mint_note_transaction(request, note_tree, selected_inputs, ctx)?;
        let submitted_tx = self.node.extensions().sign_and_submit(transaction).await?;
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
        let reserve = self.load_reserve(&request.reserve_id)?;
        let private_key = self
            .node
            .extensions()
            .get_private_key(note.owner.clone())
            .await?
            .w;
        let wallet_boxes = self.node.extensions().get_utxos().await?;
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

        let transaction = self.node.extensions().sign_and_submit(transaction).await?;
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
        let reserve_box = self.load_reserve(&request.reserve_id)?;
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
        let wallet_boxes = self.node.extensions().get_utxos().await?;
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
            oracle_box,
            buyback_box,
            receipt_contract,
            wallet_boxes,
            &tx_context,
        )?;
        let tx = self.node.extensions().sign_and_submit(tx).await?;
        Ok(tx)
    }
}
