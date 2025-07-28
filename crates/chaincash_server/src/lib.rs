//! chaincash-server
//!
//! # Introduction
//!
//! ChainCash is allowing to create money with different levels of collateralization,
//! different types of collateral and trust and so on. It is up to a user then what to
//! accept and what not. A user is identified with ChainCash server, which is
//! deciding whether to accept notes offered as a mean of payment or not. In practice,
//! there could be many users and services behind a single ChainCash server.
//! Thus we talk about client-side validation further, where is a client is a ChainCash server
//! with its individual settings. The client could be thought as a self-sovereign bank.
//!
//! # Note Acceptance Predicates
//!
//! Users of ChainCash can define predicates for note acceptance based
//! on the current holders trust and/or collateralization.
//!
//! Predicates can include:
//! - Whitelisting of current holder
//! - Blacklisting of current holder
//! - Collateralization level

pub(crate) mod acceptance;
pub(crate) mod api;
pub mod app;
pub mod error;
pub(crate) mod healthcheck;
pub(crate) mod notes;
pub(crate) mod reserves;

pub use app::Server;
pub use error::Error;

#[derive(serde::Deserialize, Debug)]
pub struct Config {
    pub url: String,
    pub port: u16,
}
