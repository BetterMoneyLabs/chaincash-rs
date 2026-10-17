use std::time::Duration;

use ergo_client::node::NodeClient;

pub use ergo_client::node::NodeError;

#[derive(serde::Deserialize, Debug)]
pub struct Config {
    url: String,
    api_key: String,
    #[serde(default)]
    wallet_password: Option<String>,
}

impl Config {
    pub fn wallet_password(&self) -> Option<&str> {
        self.wallet_password.as_deref().filter(|p| !p.is_empty())
    }
}

pub fn node_from_config(cfg: &Config) -> Result<NodeClient, NodeError> {
    Ok(NodeClient::from_url_str(
        &cfg.url,
        cfg.api_key.clone(),
        Duration::from_secs(5),
    )?)
}
