use std::borrow::BorrowMut;
use std::fmt;
use std::str::FromStr;

use chaincash_offchain::boxes::ReserveBoxSpec;
use chaincash_offchain::transactions::reserves::calculate_refund_unlock_height;
use diesel::prelude::*;
use diesel::result::{DatabaseErrorKind, Error as DieselError};
use ergo_lib::ergo_chain_types::Digest32;
use ergo_lib::ergotree_ir::chain::token::TokenId;
use serde::{Deserialize, Serialize};

use crate::{schema, ConnectionPool, Error};

/// Lifecycle state of a reserve refund.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefundStatus {
    /// Refund has been initiated on chain and is waiting for timelock expiry or automatic execution.
    Pending,
    /// Refund settlement or cancellation transaction is currently being executed.
    Executing,
    /// Refund was executed on chain and funds were withdrawn.
    Completed,
    /// Refund was cancelled before completion.
    Cancelled,
    /// Refund failed permanently (e.g. reserve balance was completely drained during the timelock).
    Failed,
}

impl RefundStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Executing => "executing",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub const fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Executing)
    }
}

impl fmt::Display for RefundStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RefundStatus {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "executing" => Ok(Self::Executing),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            other => Err(Error::InvalidRefundStatus(other.to_owned())),
        }
    }
}

/// Persisted database row for a refund in `refunds`.
#[derive(Queryable, Selectable, Clone, Debug)]
#[diesel(table_name = schema::refunds)]
pub struct RefundRecord {
    pub id: i32,
    pub reserve_id: String,
    pub reserve_box_id: Option<String>,
    pub requested_amount: i64,
    pub refunded_amount: Option<i64>,
    pub init_height: i32,
    pub settled_height: Option<i32>,
    status: String,
    pub init_tx_id: Option<String>,
    pub refund_tx_id: Option<String>,
    pub error_message: Option<String>,
}

impl RefundRecord {
    pub fn status(&self) -> Result<RefundStatus, Error> {
        RefundStatus::from_str(&self.status)
    }

    pub fn reserve_token_id(&self) -> Result<TokenId, Error> {
        Digest32::try_from(self.reserve_id.clone())
            .map(TokenId::from)
            .map_err(|_| Error::InvalidReserveTokenId(self.reserve_id.clone()))
    }

    pub fn unlock_height(&self) -> u32 {
        calculate_refund_unlock_height(self.init_height)
    }

    pub fn into_summary(self, current_height: Option<u32>) -> Result<RefundSummary, Error> {
        let status = self.status()?;
        let unlock_height = self.unlock_height();
        let (timelock_expired, blocks_remaining) = match (status, current_height) {
            (RefundStatus::Pending | RefundStatus::Executing, Some(h)) => (
                Some(h >= unlock_height),
                Some(unlock_height.saturating_sub(h)),
            ),
            (RefundStatus::Completed, _) => (Some(true), Some(0)),
            _ => (None, None),
        };

        Ok(RefundSummary {
            id: self.id,
            reserve_id: self.reserve_id,
            reserve_box_id: self.reserve_box_id,
            requested_amount: self.requested_amount,
            refunded_amount: self.refunded_amount,
            init_height: self.init_height,
            unlock_height,
            settled_height: self.settled_height,
            status,
            timelock_expired,
            blocks_remaining,
            init_tx_id: self.init_tx_id,
            refund_tx_id: self.refund_tx_id,
            error_message: self.error_message,
        })
    }
}

/// JSON-serializable view of a refund record returned by REST endpoints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundSummary {
    pub id: i32,
    pub reserve_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve_box_id: Option<String>,
    pub requested_amount: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refunded_amount: Option<i64>,
    pub init_height: i32,
    pub unlock_height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settled_height: Option<i32>,
    pub status: RefundStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timelock_expired: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks_remaining: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_tx_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refund_tx_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Optional query filters when listing refund records.
#[derive(Clone, Debug, Default)]
pub struct RefundFilter {
    pub reserve_id: Option<TokenId>,
    pub status: Option<RefundStatus>,
}

#[derive(Insertable)]
#[diesel(table_name = schema::refunds)]
struct NewRefundRow<'a> {
    reserve_id: &'a str,
    reserve_box_id: Option<&'a str>,
    requested_amount: i64,
    init_height: i32,
    status: &'a str,
    init_tx_id: Option<&'a str>,
}

const ACTIVE_STATUSES: [&str; 2] = ["pending", "executing"];
const TERMINAL_STATUSES: [&str; 3] = ["completed", "cancelled", "failed"];

pub struct RefundRepository {
    pool: ConnectionPool,
}

impl RefundRepository {
    pub(crate) fn new(pool: ConnectionPool) -> Self {
        Self { pool }
    }

    /// Creates a new `pending` refund record. Fails with [`Error::DuplicateActiveRefund`] if the
    /// reserve already has a `pending` or `executing` refund.
    pub fn create_pending(
        &self,
        reserve_id: &TokenId,
        reserve_box_id: Option<&str>,
        requested_amount: i64,
        init_height: i32,
        init_tx_id: Option<&str>,
    ) -> Result<RefundRecord, Error> {
        let mut conn = self.pool.get()?;
        let reserve_id_str = String::from(*reserve_id);
        let result = diesel::insert_into(schema::refunds::table)
            .values(&NewRefundRow {
                reserve_id: &reserve_id_str,
                reserve_box_id,
                requested_amount,
                init_height,
                status: RefundStatus::Pending.as_str(),
                init_tx_id,
            })
            .returning(RefundRecord::as_returning())
            .get_result(conn.borrow_mut());

        match result {
            Ok(record) => Ok(record),
            Err(DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, _)) => {
                Err(Error::DuplicateActiveRefund(reserve_id_str))
            }
            Err(err) => Err(Error::Diesel(err)),
        }
    }

    pub fn get_by_id(&self, id: i32) -> Result<Option<RefundRecord>, Error> {
        let mut conn = self.pool.get()?;
        Ok(schema::refunds::table
            .filter(schema::refunds::id.eq(id))
            .select(RefundRecord::as_select())
            .first(conn.borrow_mut())
            .optional()?)
    }

    pub fn active_for_reserve(&self, reserve_id: &TokenId) -> Result<Option<RefundRecord>, Error> {
        let mut conn = self.pool.get()?;
        Ok(schema::refunds::table
            .filter(schema::refunds::reserve_id.eq(String::from(*reserve_id)))
            .filter(schema::refunds::status.eq_any(ACTIVE_STATUSES))
            .select(RefundRecord::as_select())
            .first(conn.borrow_mut())
            .optional()?)
    }

    pub fn is_box_already_settled(
        &self,
        reserve_id: &TokenId,
        box_id: &str,
    ) -> Result<bool, Error> {
        let mut conn = self.pool.get()?;
        let found = schema::refunds::table
            .filter(schema::refunds::reserve_id.eq(String::from(*reserve_id)))
            .filter(schema::refunds::reserve_box_id.eq(box_id))
            .filter(schema::refunds::status.eq_any(TERMINAL_STATUSES))
            .select(schema::refunds::id)
            .first::<i32>(conn.borrow_mut())
            .optional()?;
        Ok(found.is_some())
    }

    /// Atomically transitions a `pending` refund into `executing` so concurrent workers/API calls
    /// cannot settle or cancel the same refund simultaneously.
    pub fn try_begin_execution(&self, id: i32) -> Result<Option<RefundRecord>, Error> {
        let mut conn = self.pool.get()?;
        Ok(diesel::update(schema::refunds::table)
            .filter(schema::refunds::id.eq(id))
            .filter(schema::refunds::status.eq(RefundStatus::Pending.as_str()))
            .set(schema::refunds::status.eq(RefundStatus::Executing.as_str()))
            .returning(RefundRecord::as_returning())
            .get_result(conn.borrow_mut())
            .optional()?)
    }

    /// Reverts an in-flight `executing` refund back to `pending`, optionally storing the latest
    /// transient error message for operator diagnostics.
    pub fn revert_execution_to_pending(
        &self,
        id: i32,
        error_message: Option<&str>,
    ) -> Result<RefundRecord, Error> {
        let mut conn = self.pool.get()?;
        Ok(diesel::update(schema::refunds::table)
            .filter(schema::refunds::id.eq(id))
            .filter(schema::refunds::status.eq(RefundStatus::Executing.as_str()))
            .set((
                schema::refunds::status.eq(RefundStatus::Pending.as_str()),
                schema::refunds::error_message.eq(error_message),
            ))
            .returning(RefundRecord::as_returning())
            .get_result(conn.borrow_mut())?)
    }

    /// Resets any `executing` refunds left behind by an interrupted process back to `pending`.
    pub fn recover_interrupted_executions(&self) -> Result<usize, Error> {
        let mut conn = self.pool.get()?;
        Ok(diesel::update(schema::refunds::table)
            .filter(schema::refunds::status.eq(RefundStatus::Executing.as_str()))
            .set(schema::refunds::status.eq(RefundStatus::Pending.as_str()))
            .execute(conn.borrow_mut())?)
    }

    pub fn mark_completed(
        &self,
        id: i32,
        settled_box_id: Option<&str>,
        refunded_amount: i64,
        settled_height: i32,
        refund_tx_id: &str,
    ) -> Result<RefundRecord, Error> {
        let mut conn = self.pool.get()?;
        Ok(diesel::update(schema::refunds::table)
            .filter(schema::refunds::id.eq(id))
            .filter(schema::refunds::status.eq_any(ACTIVE_STATUSES))
            .set((
                schema::refunds::status.eq(RefundStatus::Completed.as_str()),
                schema::refunds::reserve_box_id.eq(settled_box_id),
                schema::refunds::refunded_amount.eq(Some(refunded_amount)),
                schema::refunds::settled_height.eq(Some(settled_height)),
                schema::refunds::refund_tx_id.eq(Some(refund_tx_id)),
                schema::refunds::error_message.eq(None::<&str>),
            ))
            .returning(RefundRecord::as_returning())
            .get_result(conn.borrow_mut())?)
    }

    pub fn mark_cancelled(
        &self,
        id: i32,
        settled_box_id: Option<&str>,
        settled_height: Option<i32>,
        refund_tx_id: Option<&str>,
    ) -> Result<RefundRecord, Error> {
        let mut conn = self.pool.get()?;
        Ok(diesel::update(schema::refunds::table)
            .filter(schema::refunds::id.eq(id))
            .filter(schema::refunds::status.eq_any(ACTIVE_STATUSES))
            .set((
                schema::refunds::status.eq(RefundStatus::Cancelled.as_str()),
                schema::refunds::reserve_box_id.eq(settled_box_id),
                schema::refunds::settled_height.eq(settled_height),
                schema::refunds::refund_tx_id.eq(refund_tx_id),
                schema::refunds::error_message.eq(None::<&str>),
            ))
            .returning(RefundRecord::as_returning())
            .get_result(conn.borrow_mut())?)
    }

    pub fn mark_failed(
        &self,
        id: i32,
        settled_box_id: Option<&str>,
        settled_height: Option<i32>,
        reason: &str,
    ) -> Result<RefundRecord, Error> {
        let mut conn = self.pool.get()?;
        Ok(diesel::update(schema::refunds::table)
            .filter(schema::refunds::id.eq(id))
            .filter(schema::refunds::status.eq_any(ACTIVE_STATUSES))
            .set((
                schema::refunds::status.eq(RefundStatus::Failed.as_str()),
                schema::refunds::reserve_box_id.eq(settled_box_id),
                schema::refunds::settled_height.eq(settled_height),
                schema::refunds::error_message.eq(Some(reason)),
            ))
            .returning(RefundRecord::as_returning())
            .get_result(conn.borrow_mut())?)
    }

    /// Reconciles refund state against a scanned on-chain reserve box.
    ///
    /// Returns `Ok(true)` if the scanned reserve box reflects current or newer state and should
    /// update `reserves`, or `Ok(false)` if the scanned box is a stale confirmed box whose
    /// spending transaction is already tracked in `refunds` (protecting against mempool lag).
    pub fn sync_scanned_reserve(&self, reserve: &ReserveBoxSpec) -> Result<bool, Error> {
        let box_id_str = reserve.box_id().to_string();
        if let Some(init_height) = reserve.refund_height {
            if self.is_box_already_settled(&reserve.identifier, &box_id_str)? {
                return Ok(false);
            }
            let amount = reserve.refund_amount.unwrap_or(0);
            if let Some(active) = self.active_for_reserve(&reserve.identifier)? {
                if active.status()? == RefundStatus::Executing {
                    return Ok(false);
                }
                if active.reserve_box_id.as_deref() != Some(box_id_str.as_str())
                    && reserve.ergo_box().creation_height >= active.init_height.max(0) as u32
                {
                    let mut conn = self.pool.get()?;
                    diesel::update(schema::refunds::table)
                        .filter(schema::refunds::id.eq(active.id))
                        .set(schema::refunds::reserve_box_id.eq(Some(box_id_str.as_str())))
                        .execute(conn.borrow_mut())?;
                }
            } else {
                let _ = self.create_pending(
                    &reserve.identifier,
                    Some(&box_id_str),
                    amount,
                    init_height,
                    None,
                )?;
            }
            Ok(true)
        } else {
            if let Some(active) = self.active_for_reserve(&reserve.identifier)? {
                if active.status()? == RefundStatus::Executing {
                    return Ok(false);
                }
                let active_init_height = active.init_height.max(0) as u32;
                if active.init_tx_id.is_some()
                    && reserve.ergo_box().creation_height <= active_init_height
                {
                    // Pre-initiation confirmed box observed while `init_tx_id` is still in mempool
                    return Ok(false);
                }
                let settled_h = i32::try_from(reserve.ergo_box().creation_height).ok();
                self.mark_cancelled(active.id, active.reserve_box_id.as_deref(), settled_h, None)?;
            }
            Ok(true)
        }
    }

    /// Lists refund records matching `filter`, ordered newest first (`id DESC`).
    pub fn list(&self, filter: RefundFilter) -> Result<Vec<RefundRecord>, Error> {
        let mut conn = self.pool.get()?;
        let mut query = schema::refunds::table
            .order(schema::refunds::id.desc())
            .into_boxed();
        if let Some(reserve_id) = filter.reserve_id {
            query = query.filter(schema::refunds::reserve_id.eq(String::from(reserve_id)));
        }
        if let Some(status) = filter.status {
            query = query.filter(schema::refunds::status.eq(status.as_str()));
        }
        Ok(query
            .select(RefundRecord::as_select())
            .load(conn.borrow_mut())?)
    }
}

#[cfg(test)]
mod tests {
    use chaincash_offchain::transactions::reserves::REFUND_TIMELOCK_BLOCKS;
    use ergo_lib::{
        chain::ergo_box::box_builder::ErgoBoxCandidateBuilder,
        ergotree_interpreter::sigma_protocol::private_input::DlogProverInput,
        ergotree_ir::chain::{
            address::{Address, NetworkPrefix},
            ergo_box::{box_value::BoxValue, ErgoBox, NonMandatoryRegisterId},
            token::Token,
        },
    };

    use super::*;
    use crate::{ChainCashStore, Update};

    fn test_store() -> ChainCashStore {
        let store = ChainCashStore::open_in_memory().unwrap();
        store.update().unwrap();
        store
    }

    fn parse_token_id(hex: &str) -> TokenId {
        TokenId::from(Digest32::try_from(hex.to_owned()).unwrap())
    }

    fn reserve_id_1() -> TokenId {
        parse_token_id("161A3A5250655368566D597133743677397A24432646294A404D635166546A57")
    }

    fn reserve_id_2() -> TokenId {
        parse_token_id("4b2d8b7beb3eaac8234d9e61792d270898a43934d6a27275e4f3a044609c9f2a")
    }

    fn make_reserve_spec(
        token_id: TokenId,
        creation_height: u32,
        pending: Option<(i32, i64)>,
        index: u16,
    ) -> ReserveBoxSpec {
        let sk = DlogProverInput::random();
        let pk = *sk.public_image().h.clone();
        let script = Address::P2Pk(sk.public_image()).script().unwrap();
        let _ = NetworkPrefix::Mainnet;
        let mut builder = ErgoBoxCandidateBuilder::new(
            BoxValue::new(5_000_000_000).unwrap(),
            script,
            creation_height,
        );
        builder.set_register_value(NonMandatoryRegisterId::R4, pk.into());
        if let Some((h, amt)) = pending {
            builder.set_register_value(NonMandatoryRegisterId::R5, h.into());
            builder.set_register_value(NonMandatoryRegisterId::R6, amt.into());
        }
        builder.add_token(Token {
            token_id,
            amount: 1.try_into().unwrap(),
        });
        let tx_id = ergo_lib::chain::transaction::TxId::zero();
        let ergo_box =
            ErgoBox::from_box_candidate(&builder.build().unwrap(), tx_id, index).unwrap();
        ReserveBoxSpec::try_from(&ergo_box).unwrap()
    }

    #[test]
    fn test_lifecycle_pending_executing_completed_and_summary() {
        let store = test_store();
        let repo = store.refunds();

        let rec = repo
            .create_pending(
                &reserve_id_1(),
                Some("box_init_1"),
                3_000_000_000,
                100_000,
                Some("tx_init_1"),
            )
            .unwrap();
        assert_eq!(rec.status().unwrap(), RefundStatus::Pending);
        assert_eq!(rec.unlock_height(), 100_000 + REFUND_TIMELOCK_BLOCKS);

        let early_summary = rec.clone().into_summary(Some(105_000)).unwrap();
        assert_eq!(early_summary.timelock_expired, Some(false));
        assert_eq!(
            early_summary.blocks_remaining,
            Some(100_000 + REFUND_TIMELOCK_BLOCKS - 105_000)
        );

        // First caller wins execution claim; concurrent second caller gets None
        let exec = repo.try_begin_execution(rec.id).unwrap().unwrap();
        assert_eq!(exec.status().unwrap(), RefundStatus::Executing);
        assert!(repo.try_begin_execution(rec.id).unwrap().is_none());

        // Transient error reverts back to Pending while recording diagnostic message
        let retried = repo
            .revert_execution_to_pending(rec.id, Some("node connection timeout"))
            .unwrap();
        assert_eq!(retried.status().unwrap(), RefundStatus::Pending);
        assert_eq!(
            retried.error_message.as_deref(),
            Some("node connection timeout")
        );

        // Re-claim and complete
        let _ = repo.try_begin_execution(rec.id).unwrap().unwrap();
        let done = repo
            .mark_completed(
                rec.id,
                Some("box_init_1"),
                2_999_000_000,
                114_400,
                "tx_complete_1",
            )
            .unwrap();
        assert_eq!(done.status().unwrap(), RefundStatus::Completed);
        assert_eq!(done.refunded_amount, Some(2_999_000_000));
        assert_eq!(done.settled_height, Some(114_400));
        assert_eq!(done.refund_tx_id.as_deref(), Some("tx_complete_1"));
        assert_eq!(done.error_message, None);
        assert!(repo.active_for_reserve(&reserve_id_1()).unwrap().is_none());
        assert!(repo
            .is_box_already_settled(&reserve_id_1(), "box_init_1")
            .unwrap());
    }

    #[test]
    fn test_duplicate_active_refund_prevented_and_allowed_after_settlement() {
        let store = test_store();
        let repo = store.refunds();

        let first = repo
            .create_pending(&reserve_id_1(), Some("b1"), 1_000_000_000, 500, Some("tx1"))
            .unwrap();
        assert!(matches!(
            repo.create_pending(&reserve_id_1(), Some("b2"), 1_000_000_000, 501, Some("tx2")),
            Err(Error::DuplicateActiveRefund(_))
        ));

        // Different reserve can have its own active refund simultaneously
        let other = repo
            .create_pending(
                &reserve_id_2(),
                Some("b_other"),
                2_000_000_000,
                600,
                Some("tx_other"),
            )
            .unwrap();
        assert_eq!(other.status().unwrap(), RefundStatus::Pending);

        // Cancelling frees reserve_id_1 for a subsequent refund while keeping history
        repo.mark_cancelled(first.id, Some("b1"), Some(550), Some("tx_cancel"))
            .unwrap();
        let second = repo
            .create_pending(&reserve_id_1(), Some("b3"), 1_500_000_000, 700, Some("tx3"))
            .unwrap();
        assert_eq!(second.status().unwrap(), RefundStatus::Pending);

        // Filtering by reserve and status
        let r1_all = repo
            .list(RefundFilter {
                reserve_id: Some(reserve_id_1()),
                status: None,
            })
            .unwrap();
        assert_eq!(r1_all.len(), 2);
        assert_eq!(r1_all[0].id, second.id);
        assert_eq!(r1_all[1].id, first.id);

        let cancelled_only = repo
            .list(RefundFilter {
                reserve_id: None,
                status: Some(RefundStatus::Cancelled),
            })
            .unwrap();
        assert_eq!(cancelled_only.len(), 1);
        assert_eq!(cancelled_only[0].id, first.id);
    }

    #[test]
    fn test_recover_interrupted_executions_and_failed_state() {
        let store = test_store();
        let repo = store.refunds();

        let r = repo
            .create_pending(&reserve_id_1(), Some("b1"), 1_000_000_000, 100, None)
            .unwrap();
        let _ = repo.try_begin_execution(r.id).unwrap().unwrap();
        assert_eq!(repo.recover_interrupted_executions().unwrap(), 1);
        assert_eq!(
            repo.get_by_id(r.id).unwrap().unwrap().status().unwrap(),
            RefundStatus::Pending
        );

        let failed = repo
            .mark_failed(r.id, Some("b1"), Some(14_500), "reserve fully drained")
            .unwrap();
        assert_eq!(failed.status().unwrap(), RefundStatus::Failed);
        assert_eq!(
            failed.error_message.as_deref(),
            Some("reserve fully drained")
        );
    }

    #[test]
    fn test_sync_scanned_reserve_mempool_protection_and_external_events() {
        let store = test_store();
        let repo = store.refunds();

        // 1. Server initiates refund at height 1000 (in mempool)
        let init_spec = make_reserve_spec(reserve_id_1(), 1000, Some((1000, 2_000_000_000)), 1);
        let init_box_id = init_spec.box_id().to_string();
        let rec = repo
            .create_pending(
                &reserve_id_1(),
                Some(&init_box_id),
                2_000_000_000,
                1000,
                Some("tx_init"),
            )
            .unwrap();

        // Pre-initiation confirmed box (creation_height 950 <= 1000, R5 = None) must NOT cancel pending refund
        let pre_init_confirmed = make_reserve_spec(reserve_id_1(), 950, None, 0);
        assert!(!repo.sync_scanned_reserve(&pre_init_confirmed).unwrap());
        assert_eq!(
            repo.get_by_id(rec.id).unwrap().unwrap().status().unwrap(),
            RefundStatus::Pending
        );

        // Mid-timelock redemption box (creation_height 1200, still carrying R5=1000) updates reserve_box_id
        let redeemed_spec = make_reserve_spec(reserve_id_1(), 1200, Some((1000, 2_000_000_000)), 2);
        let redeemed_box_id = redeemed_spec.box_id().to_string();
        assert!(repo.sync_scanned_reserve(&redeemed_spec).unwrap());
        assert_eq!(
            repo.get_by_id(rec.id)
                .unwrap()
                .unwrap()
                .reserve_box_id
                .as_deref(),
            Some(redeemed_box_id.as_str())
        );

        // Complete refund spending `redeemed_box_id` (in mempool)
        repo.mark_completed(
            rec.id,
            Some(&redeemed_box_id),
            2_000_000_000,
            15_400,
            "tx_complete",
        )
        .unwrap();

        // If scanner re-observes `redeemed_spec` before `tx_complete` mines, it is ignored (no duplicate row!)
        assert!(!repo.sync_scanned_reserve(&redeemed_spec).unwrap());
        assert_eq!(repo.list(RefundFilter::default()).unwrap().len(), 1);

        // 2. Externally initiated refund discovered on reserve_id_2 is backfilled immediately
        let ext_spec = make_reserve_spec(reserve_id_2(), 2000, Some((2000, 1_500_000_000)), 3);
        assert!(repo.sync_scanned_reserve(&ext_spec).unwrap());
        let ext_active = repo.active_for_reserve(&reserve_id_2()).unwrap().unwrap();
        assert_eq!(ext_active.requested_amount, 1_500_000_000);
        assert_eq!(ext_active.init_tx_id, None);

        // Externally cancelled on chain at height 2100 -> automatically marked Cancelled
        let ext_cancelled_spec = make_reserve_spec(reserve_id_2(), 2100, None, 4);
        assert!(repo.sync_scanned_reserve(&ext_cancelled_spec).unwrap());
        assert_eq!(
            repo.get_by_id(ext_active.id)
                .unwrap()
                .unwrap()
                .status()
                .unwrap(),
            RefundStatus::Cancelled
        );
    }
}
