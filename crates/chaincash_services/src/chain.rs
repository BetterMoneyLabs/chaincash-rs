//! Blockchain lookups that work both with and without the node's extra index
//! (`/blockchain/*` endpoints, see <https://docs.ergoplatform.com/node/conf/conf-node/#extra-index>).
//!
//! When the node has extra indexing enabled the indexer API is used directly. Otherwise lookups are
//! served from node scans (`/scan/unspentBoxes`, `/scan/spentBoxes`) and block data
//! (`/blocks/at/{height}`, `/blocks/{id}/transactions`), which every node exposes.

use std::{collections::HashMap, sync::Arc};

use ergo_client::node::{
    endpoints::{
        blockchain::{IndexQuery, SortDirection},
        scan::{Scan, ScanBox, ScanQuery, TrackingRule},
    },
    NodeApiError, NodeClient, NodeError,
};
use ergo_lib::{
    chain::transaction::{Transaction, TxId},
    ergotree_ir::{
        chain::{
            address::NetworkAddress,
            ergo_box::{BoxId, ErgoBox},
            token::TokenId,
        },
        ergo_tree::ErgoTree,
    },
};
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Url,
};
use tokio::sync::OnceCell;
use tracing::{info, warn};

/// Maximum number of boxes the node returns from a single scan box request
const SCAN_PAGE_LIMIT: u32 = 2500;

#[derive(Clone)]
pub struct ChainSource {
    node: NodeClient,
    http: reqwest::Client,
    url: Url,
    indexed: Arc<OnceCell<bool>>,
}

impl ChainSource {
    pub fn new(node: NodeClient, api_key: &str) -> Result<Self, NodeError> {
        let mut headers = HeaderMap::new();
        let mut key = HeaderValue::from_str(api_key).map_err(|e| NodeError::InvalidApiKey {
            source: e,
            key: api_key.to_owned(),
        })?;
        key.set_sensitive(true);
        headers.insert("api_key", key);
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(NodeError::Client)?;
        Ok(Self {
            url: node.endpoints().url().clone(),
            node,
            http,
            indexed: Arc::new(OnceCell::new()),
        })
    }

    pub fn node(&self) -> &NodeClient {
        &self.node
    }

    /// Returns true if the node has extra indexing enabled. The result is cached after the first
    /// successful check.
    pub async fn has_indexer(&self) -> Result<bool, NodeError> {
        self.indexed
            .get_or_try_init(|| async {
                match self.node.endpoints().blockchain()?.indexed_height().await {
                    Ok(_) => Ok(true),
                    // Node could not be reached, don't cache anything
                    Err(e @ NodeError::Http(_)) => Err(e),
                    Err(e) => {
                        info!("Node extra indexing not available ({e}), falling back to scans");
                        Ok(false)
                    }
                }
            })
            .await
            .copied()
    }

    /// Fetch a transaction by id. `inclusion_height` is required when the node is not indexed
    pub async fn transaction(
        &self,
        tx_id: &TxId,
        inclusion_height: Option<u32>,
    ) -> Result<Option<Transaction>, NodeError> {
        let blocks = self.node.endpoints().blocks()?;
        let block_id = if self.has_indexer().await? {
            self.node
                .endpoints()
                .blockchain()?
                .get_transaction_by_id(tx_id)
                .await?
                .block_id
        } else {
            let Some(height) = inclusion_height else {
                return Ok(None);
            };
            match blocks.block_at_height(height).await? {
                Some(block_id) => block_id,
                None => return Ok(None),
            }
        };
        Ok(blocks
            .transactions(&block_id)
            .await?
            .into_iter()
            .find(|tx| tx.id() == *tx_id))
    }

    /// Build a lookup table for boxes (spent and unspent) tracked by the given scans.
    /// Returns an empty table if the node is indexed, since box lookups go through the indexer then
    pub async fn box_index(&self, scan_ids: &[i32]) -> Result<BoxIndex, NodeError> {
        let mut boxes = HashMap::new();
        if !self.has_indexer().await? {
            for &scan_id in scan_ids {
                let scan_id = scan_id as u32;
                let unspent = self
                    .node
                    .extensions()
                    .get_all_unspent_boxes(scan_id, false)
                    .await?;
                let spent = self.get_all_spent_boxes(scan_id).await?;
                boxes.extend(
                    unspent
                        .into_iter()
                        .chain(spent)
                        .map(|b| (b.ergo_box.box_id(), b)),
                );
            }
        }
        Ok(BoxIndex { boxes })
    }

    /// Look up a box (spent or unspent) by id. Returns the box and its inclusion height if known.
    /// Without an indexer only boxes in `index` can be found
    pub async fn box_by_id(
        &self,
        box_id: &BoxId,
        index: &BoxIndex,
    ) -> Result<Option<(ErgoBox, Option<u32>)>, NodeError> {
        if self.has_indexer().await? {
            let indexed = self
                .node
                .endpoints()
                .blockchain()?
                .get_box_by_id(box_id)
                .await?;
            Ok(Some((indexed.ergo_box, None)))
        } else {
            Ok(index
                .boxes
                .get(box_id)
                .map(|b| (b.ergo_box.clone(), b.inclusion_height)))
        }
    }

    /// Make sure a scan tracking `token_id` is registered when the node is not indexed.
    /// Returns true if a new scan was registered (and the wallet should be rescanned)
    pub async fn ensure_token_scan(
        &self,
        name: &str,
        token_id: TokenId,
    ) -> Result<bool, NodeError> {
        if self.has_indexer().await? || self.token_scan_id(token_id).await?.is_some() {
            return Ok(false);
        }
        let scan = Scan {
            scan_name: name.into(),
            wallet_interaction: "off".into(),
            tracking_rule: TrackingRule::ContainsAsset { asset_id: token_id },
            remove_offchain: true,
        };
        let scan_id = self.node.endpoints().scan()?.register(&scan).await?;
        info!("Registered scan {scan_id} ({name}) for token {token_id:?}");
        Ok(true)
    }

    /// Find the latest unspent box holding `token_id` (e.g. an oracle or buyback NFT)
    pub async fn unspent_box_by_token(
        &self,
        token_id: TokenId,
    ) -> Result<Option<ErgoBox>, NodeError> {
        if self.has_indexer().await? {
            return Ok(self
                .node
                .endpoints()
                .blockchain()?
                .get_unspent_boxes_by_token_id(
                    &String::from(token_id),
                    IndexQuery {
                        offset: 0,
                        limit: 1,
                        sort_direction: SortDirection::Descending,
                        include_unconfirmed: true,
                    },
                )
                .await?
                .into_iter()
                .next()
                .map(|b| b.ergo_box));
        }
        let Some(scan_id) = self.token_scan_id(token_id).await? else {
            warn!("No scan registered for token {token_id:?}");
            return Ok(None);
        };
        let mut boxes = self
            .node
            .extensions()
            .get_all_unspent_boxes(scan_id, true)
            .await?;
        // Newest first; unconfirmed boxes (no inclusion height) are the most recent
        boxes.sort_by_key(|b| std::cmp::Reverse(b.inclusion_height.unwrap_or(u32::MAX)));
        // Scans can report spent boxes as unspent, e.g. when the node skipped blocks during a
        // wallet rescan. Confirm against the UTXO set (+ mempool) before using a box
        for scan_box in boxes {
            if self.is_unspent(&scan_box.ergo_box.box_id()).await? {
                return Ok(Some(scan_box.ergo_box));
            }
        }
        Ok(None)
    }

    /// Check whether a box is in the node's UTXO set or created by a mempool transaction and not
    /// spent there
    async fn is_unspent(&self, box_id: &BoxId) -> Result<bool, NodeError> {
        let mut url = self.url.clone();
        url.path_segments_mut()
            .map_err(|_| NodeError::BaseUrl)?
            .extend(&["utxo", "withPool", "byId", &box_id.to_string()]);
        let response = self.http.get(url).send().await.map_err(NodeError::Http)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        Self::process_response::<ErgoBox>(response).await?;
        Ok(true)
    }

    /// Compile ErgoScript source to an ErgoTree using the node's `/script/p2sAddress` endpoint.
    ///
    /// Ergo node 6.x requires a `treeVersion` field, which `ergo_client` does not send. Version 0
    /// is used so the trees stay identical to the contracts deployed on-chain (older nodes ignore
    /// the field).
    pub async fn compile_contract(&self, source: &str) -> Result<ErgoTree, NodeError> {
        #[derive(serde::Deserialize)]
        struct P2sAddressResponse {
            address: NetworkAddress,
        }
        let mut url = self.url.clone();
        url.path_segments_mut()
            .map_err(|_| NodeError::BaseUrl)?
            .extend(&["script", "p2sAddress"]);
        let response = self
            .http
            .post(url)
            .json(&serde_json::json!({ "source": source, "treeVersion": 0 }))
            .send()
            .await
            .map_err(NodeError::Http)?;
        let address = Self::process_response::<P2sAddressResponse>(response)
            .await?
            .address;
        address
            .address()
            .script()
            .map_err(|e| NodeError::BadRequest(format!("Invalid compiled contract: {e}")))
    }

    async fn process_response<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, NodeError> {
        if response.status().is_success() {
            response.json().await.map_err(NodeError::Json)
        } else {
            let err: NodeApiError = response.json().await.map_err(NodeError::Json)?;
            Err(NodeError::BadRequest(err.detail))
        }
    }

    async fn token_scan_id(&self, token_id: TokenId) -> Result<Option<u32>, NodeError> {
        let rule = TrackingRule::ContainsAsset { asset_id: token_id };
        Ok(self
            .node
            .endpoints()
            .scan()?
            .list_all()
            .await?
            .into_iter()
            .find(|s| s.scan.tracking_rule == rule)
            .map(|s| s.scan_id))
    }

    async fn spent_boxes(
        &self,
        scan_id: u32,
        query: &ScanQuery,
    ) -> Result<Vec<ScanBox>, NodeError> {
        let mut url = self.url.clone();
        url.path_segments_mut()
            .map_err(|_| NodeError::BaseUrl)?
            .extend(&["scan", "spentBoxes", &scan_id.to_string()]);
        let response = self
            .http
            .get(url)
            .query(query)
            .send()
            .await
            .map_err(NodeError::Http)?;
        Self::process_response(response).await
    }

    async fn get_all_spent_boxes(&self, scan_id: u32) -> Result<Vec<ScanBox>, NodeError> {
        let mut query = ScanQuery {
            min_confirmations: 0,
            max_confirmations: -1,
            min_inclusion_height: 0,
            max_inclusion_height: -1,
            limit: SCAN_PAGE_LIMIT,
            offset: 0,
        };
        let mut boxes = vec![];
        loop {
            let page = self.spent_boxes(scan_id, &query).await?;
            if page.is_empty() {
                break;
            }
            query.offset += page.len() as u32;
            boxes.extend(page);
        }
        Ok(boxes)
    }
}

/// Boxes tracked by node scans, keyed by box id. Used for box lookups when the node is not indexed
#[derive(Default)]
pub struct BoxIndex {
    boxes: HashMap<BoxId, ScanBox>,
}

/// Tests against a live node at localhost:9053 (API key "hello"). Run with
/// `cargo test -p chaincash_services -- --ignored`
#[cfg(test)]
mod tests {
    use super::*;
    use chaincash_offchain::{contracts::RESERVE_CONTRACT, oracle::oracle_nft};
    use ergo_lib::ergotree_ir::chain::address::AddressEncoder;

    /// Reserve contract address deployed on mainnet
    const MAINNET_RESERVE_ADDRESS: &str = "2Ra2gepQYdZrJBu1rYoYXaZtkNKfpQYgRNLbafkgtzhmVNRsbQmqmCAUJSJWsGJWCJm4JQgAbA8JNJRcEAEfgjskavH2BXFCNeNvqUwupMUZqPtMRNHBZFe7BG6Zc6wqYgG624R3jym5dkNEeZXzYMJZH6wwCyvYpGVGyMqwE8Lb7qdMJK1VXBPfs4RkFKAxPAeSiCC6ndKrgBPJ3UoNpK79UdLfZix6LTgq5EGmzZfxaXAZSy33P89zvNhxT9VxJUNgPc8ucUEc3yhGJGAZoMqnWdAVkh8CATVB3qRLp3BoFoRV89F97N7LZcH3GjxnbtDToPsnBo7791x2eu4q9DCFesFCsWsdo8kUbruoPW98nqya9wdoNAGSvXwzjhU2R7desqGkWwoQjWiT2mG6zNrDXYkR2dq5VTCFcUZPuANF3p1Ymb8ADGM3cTvnLFSPGD6v7jWKPZhzun6FRHEGPnjmA83LVXP8zrtmtT8sKCZ5UpBx3Dw1U92ghvYEQ722bJwHSC4XaEfkXVeYbuwHAfY9E9cDRivZyQ6Zdp3PWFfeYqmRgDDNEFtu7FhuooR4eKceNkWwMbEg7bqU7WEGap9Djde9fhGKR3TjxYZD8hCKyuxmQSFPcz8wncqmqgaeNkHQtty4TuefPSp5ebuxdrqmjt3T7Goyv7f9pBwsEjkVLhLmutDZdzR5813USsf5qV4v2q1Q7LnCcJ6vTaEacGzbuuYsBcUoVZnBySXJj2UbTJt6qbShh5TAhAi1VjC7kSd4Uiye2hZFM1zbECJDJBPjbGEWXcvnAaQQnS3qDyc1JqcZWXWJxpyZQrnvB1qUVbagbGPbdHKX85nQJsdC9PqRafHxgb2vH57UMrEpMBWT9Mj9YGxGSz7L5cGyJnvdaQcm5942k6GCDFxg5TW8p8mSuhNZi5GUJCBwNiZLBfT844WtTNNTSAe63CtA3tCCnnWYxtXSqQsdxB59jVe";

    fn chain() -> ChainSource {
        let node = NodeClient::from_url_str(
            "http://localhost:9053",
            "hello".into(),
            std::time::Duration::from_secs(30),
        )
        .unwrap();
        ChainSource::new(node, "hello").unwrap()
    }

    async fn is_mainnet(chain: &ChainSource) -> bool {
        chain
            .node()
            .endpoints()
            .root()
            .unwrap()
            .info()
            .await
            .unwrap()
            .network
            == "mainnet"
    }

    #[tokio::test]
    #[ignore = "requires a running mainnet node"]
    async fn compiled_reserve_contract_matches_mainnet() {
        let chain = chain();
        let tree = chain.compile_contract(RESERVE_CONTRACT).await.unwrap();
        let expected = AddressEncoder::unchecked_parse_address_from_str(MAINNET_RESERVE_ADDRESS)
            .unwrap()
            .script()
            .unwrap();
        assert_eq!(tree, expected);
    }

    #[tokio::test]
    #[ignore = "requires a running node"]
    async fn oracle_box_is_unspent() {
        let chain = chain();
        println!("indexer available: {}", chain.has_indexer().await.unwrap());
        let mainnet = is_mainnet(&chain).await;
        chain
            .ensure_token_scan("Chaincash oracle scan", oracle_nft(mainnet))
            .await
            .unwrap();
        let oracle_box = chain
            .unspent_box_by_token(oracle_nft(mainnet))
            .await
            .unwrap()
            .expect("oracle box not found");
        assert!(chain.is_unspent(&oracle_box.box_id()).await.unwrap());
        assert_eq!(
            oracle_box.tokens.as_ref().unwrap().first().token_id,
            oracle_nft(mainnet)
        );
    }
}
