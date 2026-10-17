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
use ergo_client::node::endpoints::wallet::StatusResponse;
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
use serde::Deserialize;
use thiserror::Error;

use crate::compiler::Compiler;

#[derive(Debug, Error)]
pub enum TransactionServiceError {
    #[error("Change address not set in wallet")]
    ChangeAddressNotSet,

    #[error("Wallet is locked and no wallet password is configured")]
    WalletLocked,

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
    wallet_password: Option<&'a str>,
}

impl<'a> TransactionService<'a> {
    pub fn new(node: &'a NodeClient, store: &'a ChainCashStore, compiler: &'a Compiler) -> Self {
        Self::with_wallet_password(node, store, compiler, None)
    }

    pub fn with_wallet_password(
        node: &'a NodeClient,
        store: &'a ChainCashStore,
        compiler: &'a Compiler,
        wallet_password: Option<&'a str>,
    ) -> Self {
        Self {
            node,
            store,
            compiler,
            wallet_password,
        }
    }

    pub async fn ensure_wallet_unlocked(&self) -> Result<StatusResponse, TransactionServiceError> {
        let wallet = self.node.endpoints().wallet()?;
        let status = wallet.status().await?;
        if status.is_unlocked {
            return Ok(status);
        }
        let password = self
            .wallet_password
            .filter(|p| !p.is_empty())
            .ok_or(TransactionServiceError::WalletLocked)?;
        wallet.unlock(password.to_string()).await?;
        Ok(wallet.status().await?)
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
        let wallet_status = self.ensure_wallet_unlocked().await?;
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
        let reserve = self
            .store
            .reserves()
            .get_reserve_by_identifier(&request.reserve_id)?;
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
        let reserve = self
            .store
            .reserves()
            .get_reserve_by_identifier(&request.reserve_id)?;
        let tx_context = self.get_tx_ctx().await?;
        let private_key = self
            .node
            .extensions()
            .get_private_key(note.owner.clone())
            .await?
            .w;
        let wallet_boxes = self.node.extensions().get_utxos().await?;
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
        let tx_context = self.get_tx_ctx().await?;
        let wallet_boxes = self.node.extensions().get_utxos().await?;
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
        let tx = self.node.extensions().sign_and_submit(tx).await?;
        Ok(tx)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    async fn spawn_mock_wallet_node(initially_unlocked: bool) -> (String, Arc<AtomicBool>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let unlocked = Arc::new(AtomicBool::new(initially_unlocked));
        let unlocked_clone = unlocked.clone();

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 4096];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.starts_with("POST /wallet/unlock") {
                    unlocked_clone.store(true, Ordering::SeqCst);
                    "\"OK\"".to_string()
                } else {
                    let is_unlocked = unlocked_clone.load(Ordering::SeqCst);
                    let change_addr = if is_unlocked { "9fChangeAddr" } else { "" };
                    format!(
                        "{{\"isInitialized\":true,\"isUnlocked\":{is_unlocked},\"changeAddress\":\"{change_addr}\",\"walletHeight\":100,\"error\":\"\"}}"
                    )
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            }
        });

        (format!("http://{addr}"), unlocked)
    }

    #[tokio::test]
    async fn unlocks_locked_wallet_when_password_configured() {
        let (url, unlocked) = spawn_mock_wallet_node(false).await;
        let node =
            NodeClient::from_url_str(&url, "hello".to_string(), Duration::from_secs(5)).unwrap();
        let store = ChainCashStore::open_in_memory().unwrap();
        let state = crate::ServerState::with_wallet_password(
            node,
            store,
            vec![],
            Some("secret".to_string()),
        );

        let status = state.ensure_wallet_unlocked().await.unwrap();
        assert!(status.is_unlocked);
        assert_eq!(status.change_address, "9fChangeAddr");
        assert!(unlocked.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn errors_when_wallet_locked_without_password() {
        let (url, _) = spawn_mock_wallet_node(false).await;
        let node =
            NodeClient::from_url_str(&url, "hello".to_string(), Duration::from_secs(5)).unwrap();
        let store = ChainCashStore::open_in_memory().unwrap();
        let state = crate::ServerState::new(node, store, vec![]);

        let err = state.ensure_wallet_unlocked().await.unwrap_err();
        assert!(matches!(err, TransactionServiceError::WalletLocked));
    }
}
