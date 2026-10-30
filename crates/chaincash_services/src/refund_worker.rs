use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chaincash_offchain::boxes::ReserveBoxSpec;
use chaincash_offchain::transactions::reserves::calculate_refund_unlock_height;
use chaincash_store::refunds::RefundSummary;
use chaincash_store::ChainCashStore;
use ergo_client::node::NodeError;
use ergo_lib::ergotree_ir::chain::token::TokenId;
use thiserror::Error;
use tracing::{info, warn};

use crate::transaction::TransactionServiceError;
use crate::ServerState;

const WORKER_RETRY_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug, Error)]
pub enum RefundWorkerError {
    #[error("Node error: {0}")]
    Node(#[from] NodeError),

    #[error("Store error: {0}")]
    Store(#[from] chaincash_store::Error),

    #[error("Transaction service error: {0}")]
    TransactionService(#[from] TransactionServiceError),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefundPassReport {
    pub completed: Vec<RefundSummary>,
    pub failed_reserves: Vec<String>,
}

pub trait RefundExecutionBackend {
    fn store(&self) -> &ChainCashStore;

    fn current_height(&self) -> impl Future<Output = Result<u32, RefundWorkerError>> + Send;

    fn wallet_reserves(
        &self,
    ) -> impl Future<Output = Result<Vec<ReserveBoxSpec>, RefundWorkerError>> + Send;

    fn complete_reserve_refund(
        &self,
        reserve_id: &TokenId,
    ) -> impl Future<Output = Result<RefundSummary, TransactionServiceError>> + Send;
}

impl RefundExecutionBackend for ServerState {
    fn store(&self) -> &ChainCashStore {
        &self.store
    }

    async fn current_height(&self) -> Result<u32, RefundWorkerError> {
        Ok(ServerState::current_height(self).await?)
    }

    async fn wallet_reserves(&self) -> Result<Vec<ReserveBoxSpec>, RefundWorkerError> {
        let pubkeys = self.wallet_pubkeys().await?;
        Ok(self.store.reserves().reserve_boxes_by_pubkeys(&pubkeys)?)
    }

    async fn complete_reserve_refund(
        &self,
        reserve_id: &TokenId,
    ) -> Result<RefundSummary, TransactionServiceError> {
        let res = self.tx_service().complete_refund(reserve_id).await?;
        Ok(res.refund.into_summary(Some(res.current_height))?)
    }
}

/// Evaluates all wallet-owned reserves against the current chain height, reconciles any
/// externally announced on-chain refunds into persistent storage, and automatically executes
/// refunds whose timelock has expired.
pub async fn process_due_refunds<B: RefundExecutionBackend>(
    backend: &B,
) -> Result<RefundPassReport, RefundWorkerError> {
    let current_height = backend.current_height().await?;
    let reserves = backend.wallet_reserves().await?;
    let mut report = RefundPassReport::default();

    for reserve in reserves {
        let box_id_str = reserve.box_id().to_string();
        if !backend.store().refunds().sync_scanned_reserve(&reserve)? {
            continue;
        }
        let Some(init_height) = reserve.refund_height else {
            continue;
        };
        let unlock_height = calculate_refund_unlock_height(init_height);
        if current_height < unlock_height {
            continue;
        }
        if backend
            .store()
            .refunds()
            .is_box_already_settled(&reserve.identifier, &box_id_str)?
        {
            continue;
        }

        let reserve_id_str = String::from(reserve.identifier);
        match backend.complete_reserve_refund(&reserve.identifier).await {
            Ok(summary) => {
                info!(
                    "Automatically completed refund #{} for reserve {} (amount={} nanoERG, tx={})",
                    summary.id,
                    summary.reserve_id,
                    summary.refunded_amount.unwrap_or_default(),
                    summary.refund_tx_id.as_deref().unwrap_or("unknown"),
                );
                report.completed.push(summary);
            }
            Err(err) => {
                warn!(
                    "Automatic refund completion for reserve {} failed at height {}: {}",
                    reserve_id_str, current_height, err
                );
                report.failed_reserves.push(reserve_id_str);
            }
        }
    }

    Ok(report)
}

async fn run_refund_worker_loop(state: Arc<ServerState>) {
    match state.store.refunds().recover_interrupted_executions() {
        Ok(count) if count > 0 => {
            info!("Recovered {count} interrupted refund execution(s) back to pending")
        }
        Ok(_) => {}
        Err(err) => warn!("Failed to recover interrupted refund executions: {err}"),
    }

    loop {
        if let Err(err) = process_due_refunds(state.as_ref()).await {
            warn!("Refund worker pass failed: {err}; retrying in {WORKER_RETRY_INTERVAL:?}");
            tokio::time::sleep(WORKER_RETRY_INTERVAL).await;
            continue;
        }
        if let Err(err) = crate::scanner::wait_scan_block(&state).await {
            warn!("Refund worker failed waiting for next block: {err}; retrying in {WORKER_RETRY_INTERVAL:?}");
            tokio::time::sleep(WORKER_RETRY_INTERVAL).await;
        }
    }
}

/// Spawns the background automatic refund settlement loop.
pub fn start_refund_worker(state: Arc<ServerState>) {
    tokio::spawn(run_refund_worker_loop(state));
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chaincash_offchain::transactions::reserves::REFUND_TIMELOCK_BLOCKS;
    use chaincash_offchain::transactions::TransactionError;
    use chaincash_store::refunds::{RefundFilter, RefundStatus};
    use chaincash_store::Update;
    use ergo_lib::{
        chain::{ergo_box::box_builder::ErgoBoxCandidateBuilder, transaction::TxId},
        ergo_chain_types::Digest32,
        ergotree_interpreter::sigma_protocol::private_input::DlogProverInput,
        ergotree_ir::chain::{
            address::Address,
            ergo_box::{box_value::BoxValue, ErgoBox, NonMandatoryRegisterId},
            token::Token,
        },
    };

    use super::*;

    struct MockRefundBackend {
        store: ChainCashStore,
        wallet_pubkey: ergo_lib::ergo_chain_types::EcPoint,
        height: Mutex<u32>,
        fail_transient_once: Mutex<bool>,
    }

    impl MockRefundBackend {
        fn new(initial_height: u32) -> (Self, DlogProverInput) {
            let store = ChainCashStore::open_in_memory().unwrap();
            store.update().unwrap();
            let sk = DlogProverInput::random();
            let wallet_pubkey = *sk.public_image().h.clone();
            (
                Self {
                    store,
                    wallet_pubkey,
                    height: Mutex::new(initial_height),
                    fail_transient_once: Mutex::new(false),
                },
                sk,
            )
        }

        fn set_height(&self, h: u32) {
            *self.height.lock().unwrap() = h;
        }
    }

    fn build_reserve_box(
        sk: &DlogProverInput,
        token_hex: &str,
        value: u64,
        creation_height: u32,
        pending: Option<(i32, i64)>,
        out_index: u16,
    ) -> ReserveBoxSpec {
        let pk = *sk.public_image().h.clone();
        let script = Address::P2Pk(sk.public_image()).script().unwrap();
        let mut builder =
            ErgoBoxCandidateBuilder::new(BoxValue::new(value).unwrap(), script, creation_height);
        builder.set_register_value(NonMandatoryRegisterId::R4, pk.into());
        if let Some((init_h, amt)) = pending {
            builder.set_register_value(NonMandatoryRegisterId::R5, init_h.into());
            builder.set_register_value(NonMandatoryRegisterId::R6, amt.into());
        }
        let token_id = TokenId::from(Digest32::try_from(token_hex.to_owned()).unwrap());
        builder.add_token(Token {
            token_id,
            amount: 1.try_into().unwrap(),
        });
        let ergo_box =
            ErgoBox::from_box_candidate(&builder.build().unwrap(), TxId::zero(), out_index)
                .unwrap();
        ReserveBoxSpec::try_from(&ergo_box).unwrap()
    }

    impl RefundExecutionBackend for MockRefundBackend {
        fn store(&self) -> &ChainCashStore {
            &self.store
        }

        async fn current_height(&self) -> Result<u32, RefundWorkerError> {
            Ok(*self.height.lock().unwrap())
        }

        async fn wallet_reserves(&self) -> Result<Vec<ReserveBoxSpec>, RefundWorkerError> {
            Ok(self
                .store
                .reserves()
                .reserve_boxes_by_pubkeys(std::slice::from_ref(&self.wallet_pubkey))?)
        }

        async fn complete_reserve_refund(
            &self,
            reserve_id: &TokenId,
        ) -> Result<RefundSummary, TransactionServiceError> {
            let current_height = *self.height.lock().unwrap();
            let reserve = self
                .store
                .reserves()
                .get_reserve_by_identifier(reserve_id)?;
            let active = self
                .store
                .refunds()
                .active_for_reserve(reserve_id)?
                .unwrap();
            let claimed = self
                .store
                .refunds()
                .try_begin_execution(active.id)?
                .ok_or_else(|| {
                    TransactionServiceError::RefundExecutionInProgress(String::from(*reserve_id))
                })?;

            if *self.fail_transient_once.lock().unwrap() {
                *self.fail_transient_once.lock().unwrap() = false;
                let msg = "temporary node RPC error";
                let _ = self
                    .store
                    .refunds()
                    .revert_execution_to_pending(claimed.id, Some(msg));
                return Err(TransactionServiceError::ChangeAddressNotSet);
            }

            let max_refundable = reserve.max_refundable_nanoergs();
            let requested = reserve.refund_amount.unwrap_or(0).max(0) as u64;
            let withdrawn = requested.min(max_refundable);
            if withdrawn == 0 {
                let err = TransactionError::InvalidRefundAmount {
                    requested,
                    max_refundable: 0,
                    reserve_value: *reserve.ergo_box().value.as_u64(),
                };
                let spent_box_id = reserve.box_id().to_string();
                let reason = err.to_string();
                let _ = self.store.refunds().mark_failed(
                    claimed.id,
                    Some(&spent_box_id),
                    Some(current_height as i32),
                    &reason,
                );
                return Err(TransactionServiceError::TransactionBuilding(err));
            }

            // Simulate spent reserve box replaced by clean box with reduced balance
            let spent_box_id = reserve.box_id().to_string();
            let mut next_candidate: ergo_lib::ergotree_ir::chain::ergo_box::ErgoBoxCandidate =
                reserve.ergo_box().clone().into();
            next_candidate.value =
                BoxValue::new(*reserve.ergo_box().value.as_u64() - withdrawn).unwrap();
            next_candidate.creation_height = current_height;
            next_candidate.additional_registers = {
                let mut builder = ErgoBoxCandidateBuilder::new(
                    next_candidate.value,
                    next_candidate.ergo_tree.clone(),
                    current_height,
                );
                builder
                    .set_register_value(NonMandatoryRegisterId::R4, reserve.owner.clone().into());
                builder.build().unwrap().additional_registers
            };
            let next_box = ErgoBox::from_box_candidate(&next_candidate, TxId::zero(), 99).unwrap();
            let next_spec = ReserveBoxSpec::try_from(&next_box).unwrap();
            self.store.reserves().add_or_update(&next_spec)?;

            let completed = self.store.refunds().mark_completed(
                claimed.id,
                Some(&spent_box_id),
                withdrawn as i64,
                current_height as i32,
                "auto_refund_tx_123",
            )?;
            Ok(completed.into_summary(Some(current_height))?)
        }
    }

    #[tokio::test]
    async fn test_worker_waits_for_timelock_retries_transient_error_and_completes() {
        let init_height: i32 = 500_000;
        let unlock_height = init_height as u32 + REFUND_TIMELOCK_BLOCKS;
        let (backend, sk) = MockRefundBackend::new(unlock_height - 1);

        let r_hex = "161A3A5250655368566D597133743677397A24432646294A404D635166546A57";
        let reserve = build_reserve_box(
            &sk,
            r_hex,
            8_000_000_000,
            init_height as u32,
            Some((init_height, 3_000_000_000)),
            1,
        );
        backend.store.reserves().add_or_update(&reserve).unwrap();

        // 1 block before timelock expiry -> backfills pending record, does NOT execute yet
        let early_report = process_due_refunds(&backend).await.unwrap();
        assert!(early_report.completed.is_empty());
        assert!(early_report.failed_reserves.is_empty());
        let records = backend
            .store
            .refunds()
            .list(RefundFilter::default())
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status().unwrap(), RefundStatus::Pending);

        // Advance to unlock_height, inject a one-time transient RPC failure
        backend.set_height(unlock_height);
        *backend.fail_transient_once.lock().unwrap() = true;
        let transient_report = process_due_refunds(&backend).await.unwrap();
        assert!(transient_report.completed.is_empty());
        assert_eq!(transient_report.failed_reserves.len(), 1);
        let after_transient = backend
            .store
            .refunds()
            .get_by_id(records[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(after_transient.status().unwrap(), RefundStatus::Pending);
        assert_eq!(
            after_transient.error_message.as_deref(),
            Some("temporary node RPC error")
        );

        // Next block pass succeeds automatically!
        backend.set_height(unlock_height + 1);
        let ok_report = process_due_refunds(&backend).await.unwrap();
        assert_eq!(ok_report.completed.len(), 1);
        assert_eq!(ok_report.completed[0].status, RefundStatus::Completed);
        assert_eq!(ok_report.completed[0].refunded_amount, Some(3_000_000_000));
        assert_eq!(
            ok_report.completed[0].refund_tx_id.as_deref(),
            Some("auto_refund_tx_123")
        );
        assert_eq!(
            ok_report.completed[0].settled_height,
            Some((unlock_height + 1) as i32)
        );
    }

    #[tokio::test]
    async fn test_worker_marks_drained_reserve_failed_without_infinite_loop() {
        let init_height: i32 = 200_000;
        let unlock_height = init_height as u32 + REFUND_TIMELOCK_BLOCKS;
        let (backend, sk) = MockRefundBackend::new(unlock_height);

        let r_hex = "4b2d8b7beb3eaac8234d9e61792d270898a43934d6a27275e4f3a044609c9f2a";
        let drained = build_reserve_box(
            &sk,
            r_hex,
            *BoxValue::SAFE_USER_MIN.as_u64(),
            init_height as u32,
            Some((init_height, 1_000_000_000)),
            2,
        );
        backend.store.reserves().add_or_update(&drained).unwrap();

        let first_pass = process_due_refunds(&backend).await.unwrap();
        assert_eq!(first_pass.failed_reserves.len(), 1);
        let rec = backend
            .store
            .refunds()
            .list(RefundFilter::default())
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(rec.status().unwrap(), RefundStatus::Failed);
        assert!(rec.error_message.is_some());

        // Subsequent block pass does NOT re-attempt the already-settled drained box
        backend.set_height(unlock_height + 1);
        let second_pass = process_due_refunds(&backend).await.unwrap();
        assert!(second_pass.completed.is_empty());
        assert!(second_pass.failed_reserves.is_empty());
    }
}
