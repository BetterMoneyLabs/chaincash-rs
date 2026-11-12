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

    #[error("Failed to parse serialized Ergo box: {0}")]
    SigmaParsing(#[from] ergo_lib::ergotree_ir::serialization::SigmaParsingError),
}
