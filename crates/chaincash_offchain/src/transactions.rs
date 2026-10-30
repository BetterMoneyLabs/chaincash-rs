pub mod notes;
pub mod reserves;

use ergo_lib::{
    chain::ergo_box::box_builder::ErgoBoxCandidateBuilderError,
    ergotree_ir::chain::{
        address::AddressEncoderError,
        ergo_box::box_value::BoxValueError,
        token::{TokenAmountError, TokenId},
    },
    wallet::{box_selector::BoxSelectorError, tx_builder::TxBuilderError},
};
use thiserror::Error;

use crate::{boxes, note_history::NoteHistoryError};

#[derive(Debug, Error)]
pub enum TransactionError {
    #[error("Box error: {0}")]
    BoxError(#[from] boxes::Error),

    #[error("wallet change address error: {0}")]
    ChangeAddress(String),

    #[error("box value error: {0}")]
    BoxValue(#[from] BoxValueError),

    #[error("token value error: {0}")]
    TokenValue(#[from] TokenAmountError),

    #[error("missing box: {0}")]
    MissingBox(String),

    #[error("box builder error: {0}")]
    BoxBuilder(#[from] ErgoBoxCandidateBuilderError),

    #[error("box selection error: {0}")]
    BoxSelection(#[from] BoxSelectorError),

    #[error("tx builder error: {0}")]
    TxBuilder(#[from] TxBuilderError),

    #[error("address error: {0}")]
    Address(#[from] AddressEncoderError),

    #[error("Note history error: {0}")]
    NoteHistoryError(#[from] NoteHistoryError),

    #[error("parsing error: {0}")]
    Parsing(String),

    #[error("output amount {output_amount} > input amount {input_amount}")]
    NoteAmountError {
        input_amount: u64,
        output_amount: u64,
    },

    #[error("Must top-up reserve with atleast 1 ERG, got {}", *.0 as f64 / 1_000_000_000.0)]
    TopUpAmountError(u64),

    #[error(
        "Invalid refund amount {requested} nanoERG (reserve holds {reserve_value} nanoERG, max refundable {max_refundable} nanoERG)"
    )]
    InvalidRefundAmount {
        requested: u64,
        max_refundable: u64,
        reserve_value: u64,
    },

    #[error("Reserve {0:?} already has an active refund pending")]
    RefundAlreadyActive(TokenId),

    #[error("Reserve {0:?} does not have an active refund")]
    NoActiveRefund(TokenId),

    #[error(
        "Refund on reserve {reserve_id:?} is timelocked until height {unlock_height} (current height {current_height})"
    )]
    RefundTimelockActive {
        reserve_id: TokenId,
        unlock_height: u32,
        current_height: u32,
    },

    #[error(
        "Attempted to redeem from reserve {0:?}, note does not include ownership entry for this reserve"
    )]
    ReserveEntryNotFound(TokenId),
}

pub struct TxContext {
    pub current_height: u32,
    pub change_address: String,
    pub fee: u64,
}
