use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Diesel(#[from] diesel::result::Error),

    #[error(transparent)]
    Pool(#[from] diesel::r2d2::PoolError),

    #[error("Failed to update store due to: {0}")]
    Update(&'static str),

    #[error("Failed to extract spec from box")]
    BoxSpec(#[from] chaincash_offchain::boxes::Error),

    #[error("Reserve {0} already has an active refund")]
    DuplicateActiveRefund(String),

    #[error("Invalid refund status '{0}'")]
    InvalidRefundStatus(String),

    #[error("Invalid reserve token identifier '{0}'")]
    InvalidReserveTokenId(String),
}

impl Error {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Diesel(diesel::result::Error::NotFound))
    }
}
