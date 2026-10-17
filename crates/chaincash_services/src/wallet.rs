use std::str::FromStr;

use ergo_client::node::{
    endpoints::blockchain::{IndexQuery, SortDirection},
    NodeClient, NodeError,
};
use ergo_lib::{
    chain::{
        ergo_state_context::{ErgoStateContext, Headers},
        parameters::Parameters,
        transaction::{unsigned::UnsignedTransaction, Transaction},
    },
    ergo_chain_types::{EcPoint, PreHeader},
    ergotree_interpreter::sigma_protocol::private_input::DlogProverInput,
    ergotree_ir::chain::{
        address::{Address, NetworkAddress, NetworkPrefix},
        ergo_box::ErgoBox,
    },
    wallet::{
        derivation_path::DerivationPath,
        ext_secret_key::{ExtSecretKey, ExtSecretKeyError},
        mnemonic::Mnemonic,
        secret_key::SecretKey,
        signing::TransactionContext,
        Wallet, WalletError,
    },
};
use serde::Deserialize;
use thiserror::Error;

const DEFAULT_DERIVATION_PATH: &str = "m/44'/429'/0'/0/0";

#[derive(Debug, Error)]
pub enum LocalWalletError {
    #[error("No secrets or mnemonic configured in local wallet")]
    NoSecretsConfigured,

    #[error("Invalid base16 Dlog secret key")]
    InvalidSecretKey,

    #[error("Invalid derivation path: {0}")]
    InvalidDerivationPath(String),

    #[error("Extended secret key error: {0}")]
    ExtSecretKey(#[from] ExtSecretKeyError),

    #[error("Private key not found for public key: {0:?}")]
    PrivateKeyNotFound(EcPoint),

    #[error("Insufficient headers to build ErgoStateContext (expected 10, got {0})")]
    InsufficientHeaders(usize),

    #[error("Wallet signing error: {0}")]
    Wallet(#[from] WalletError),

    #[error("Transaction context error: {0}")]
    TxContext(#[from] ergo_lib::wallet::tx_context::TransactionContextError),

    #[error("Node error: {0}")]
    Node(#[from] NodeError),
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct WalletConfig {
    /// Optional BIP-39 mnemonic phrase for deriving local wallet keys
    pub mnemonic: Option<String>,
    /// Optional mnemonic passphrase (defaults to empty string)
    #[serde(default)]
    pub mnemonic_password: String,
    /// Optional BIP-32 derivation paths (defaults to ["m/44'/429'/0'/0/0"] when mnemonic is set)
    #[serde(default)]
    pub derivation_paths: Vec<String>,
    /// Optional base16-encoded Dlog secret keys
    #[serde(default)]
    pub secrets: Vec<String>,
}

impl WalletConfig {
    pub fn is_configured(&self) -> bool {
        self.mnemonic.as_ref().is_some_and(|m| !m.trim().is_empty()) || !self.secrets.is_empty()
    }
}

#[derive(Clone)]
pub struct LocalWallet {
    secrets: Vec<DlogProverInput>,
}

impl LocalWallet {
    pub fn from_config(config: &WalletConfig) -> Result<Option<Self>, LocalWalletError> {
        if !config.is_configured() {
            return Ok(None);
        }

        let mut secrets = Vec::new();

        if let Some(mnemonic) = config.mnemonic.as_deref() {
            let trimmed = mnemonic.trim();
            if !trimmed.is_empty() {
                let seed = Mnemonic::to_seed(trimmed, &config.mnemonic_password);
                let root = ExtSecretKey::derive_master(seed)?;
                let paths: Vec<&str> = if config.derivation_paths.is_empty() {
                    vec![DEFAULT_DERIVATION_PATH]
                } else {
                    config.derivation_paths.iter().map(String::as_str).collect()
                };
                for path_str in paths {
                    let path = DerivationPath::from_str(path_str)
                        .map_err(|e| LocalWalletError::InvalidDerivationPath(format!("{e:?}")))?;
                    let derived = root.derive(path)?;
                    if let SecretKey::DlogSecretKey(dlog) = derived.secret_key() {
                        secrets.push(dlog);
                    }
                }
            }
        }

        for secret_hex in &config.secrets {
            let dlog = DlogProverInput::from_base16_str(secret_hex.trim().to_string())
                .ok_or(LocalWalletError::InvalidSecretKey)?;
            secrets.push(dlog);
        }

        if secrets.is_empty() {
            return Err(LocalWalletError::NoSecretsConfigured);
        }

        Ok(Some(Self { secrets }))
    }

    pub fn from_secrets(secrets: Vec<DlogProverInput>) -> Result<Self, LocalWalletError> {
        if secrets.is_empty() {
            return Err(LocalWalletError::NoSecretsConfigured);
        }
        Ok(Self { secrets })
    }

    pub fn pubkeys(&self) -> Vec<EcPoint> {
        self.secrets.iter().map(|s| *s.public_image().h).collect()
    }

    pub fn addresses(&self, prefix: NetworkPrefix) -> Vec<String> {
        self.secrets
            .iter()
            .map(|s| NetworkAddress::new(prefix, &Address::P2Pk(s.public_image())).to_base58())
            .collect()
    }

    pub fn change_address(&self, prefix: NetworkPrefix) -> String {
        let first = &self.secrets[0];
        NetworkAddress::new(prefix, &Address::P2Pk(first.public_image())).to_base58()
    }

    pub fn get_private_key(
        &self,
        public_key: &EcPoint,
    ) -> Result<DlogProverInput, LocalWalletError> {
        self.secrets
            .iter()
            .find(|s| *s.public_image().h == *public_key)
            .cloned()
            .ok_or_else(|| LocalWalletError::PrivateKeyNotFound(public_key.clone()))
    }

    pub fn ergo_wallet(&self) -> Wallet {
        Wallet::from_secrets(
            self.secrets
                .iter()
                .cloned()
                .map(SecretKey::DlogSecretKey)
                .collect(),
        )
    }

    pub fn sign_transaction(
        &self,
        unsigned_tx: UnsignedTransaction,
        boxes_to_spend: Vec<ErgoBox>,
        data_boxes: Vec<ErgoBox>,
        state_context: &ErgoStateContext,
    ) -> Result<Transaction, LocalWalletError> {
        let tx_context = TransactionContext::new(unsigned_tx, boxes_to_spend, data_boxes)?;
        Ok(self
            .ergo_wallet()
            .sign_transaction(tx_context, state_context, None)?)
    }

    pub async fn get_utxos(&self, node: &NodeClient) -> Result<Vec<ErgoBox>, LocalWalletError> {
        let info = node.endpoints().root()?.info().await?;
        let prefix = if info.network == "mainnet" {
            NetworkPrefix::Mainnet
        } else {
            NetworkPrefix::Testnet
        };
        let blockchain = node.endpoints().blockchain()?;
        let mut utxos = Vec::new();
        for addr in self.addresses(prefix) {
            let mut offset = 0;
            let limit = 100;
            loop {
                let indexed_boxes = blockchain
                    .unspent_by_address(
                        &addr,
                        IndexQuery {
                            offset,
                            limit,
                            sort_direction: SortDirection::Descending,
                            include_unconfirmed: false,
                        },
                    )
                    .await?;
                let fetched = indexed_boxes.len();
                utxos.extend(indexed_boxes.into_iter().map(|b| b.ergo_box));
                if (fetched as u32) < limit {
                    break;
                }
                offset += fetched as u64;
            }
        }
        Ok(utxos)
    }

    pub async fn get_utxos_summing_amount(
        &self,
        node: &NodeClient,
        nano_erg_amount: u64,
    ) -> Result<Vec<ErgoBox>, LocalWalletError> {
        let boxes = self.get_utxos(node).await?;
        let mut running_total = 0;
        let selected = boxes
            .into_iter()
            .take_while(|b| {
                let keep_taking = running_total < nano_erg_amount;
                running_total += b.value.as_u64();
                keep_taking
            })
            .collect::<Vec<_>>();
        if running_total >= nano_erg_amount {
            Ok(selected)
        } else {
            Err(NodeError::InsufficientFunds {
                requested: nano_erg_amount,
                found: running_total,
            }
            .into())
        }
    }

    pub async fn fetch_state_context(
        &self,
        node: &NodeClient,
    ) -> Result<ErgoStateContext, LocalWalletError> {
        let info = node.endpoints().root()?.info().await?;
        let end_height = info.full_height.max(0) as u32;
        let start_height = end_height.saturating_sub(10);
        let mut slice = node
            .endpoints()
            .blocks()?
            .chain_slice(start_height..end_height)
            .await?;
        slice.reverse();
        if slice.len() < 10 {
            return Err(LocalWalletError::InsufficientHeaders(slice.len()));
        }
        let headers: Headers = slice
            .into_iter()
            .take(10)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|v: Vec<_>| LocalWalletError::InsufficientHeaders(v.len()))?;
        let pre_header = PreHeader::from(headers[0].clone());
        Ok(ErgoStateContext::new(
            pre_header,
            headers,
            Parameters::default(),
        ))
    }

    pub async fn sign_and_submit(
        &self,
        node: &NodeClient,
        unsigned_tx: UnsignedTransaction,
        boxes_to_spend: Vec<ErgoBox>,
        data_boxes: Vec<ErgoBox>,
    ) -> Result<Transaction, LocalWalletError> {
        let state_context = self.fetch_state_context(node).await?;
        let signed_tx =
            self.sign_transaction(unsigned_tx, boxes_to_spend, data_boxes, &state_context)?;
        node.endpoints().transactions()?.submit(&signed_tx).await?;
        Ok(signed_tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaincash_offchain::{
        boxes::ReserveBoxSpec,
        transactions::{
            reserves::{mint_reserve_transaction, top_up_reserve_transaction, MintReserveRequest},
            TxContext,
        },
    };
    use ergo_lib::{
        chain::ergo_box::box_builder::ErgoBoxCandidateBuilder,
        ergotree_ir::{
            chain::{
                address::AddressEncoder,
                ergo_box::{box_value::BoxValue, ErgoBoxCandidate, NonMandatoryRegisterId},
                token::{Token, TokenId},
            },
            serialization::SigmaSerializable,
            sigma_protocol::sigma_boolean::ProveDlog,
        },
        wallet::box_selector::{BoxSelector, SimpleBoxSelector},
    };
    use proptest::{
        arbitrary::{any, Arbitrary},
        strategy::{Strategy, ValueTree},
        test_runner::TestRunner,
    };

    const RESERVE_ADDRESS: &str = "2Ra2gepQYdZrJBu1rYoYXaZtkNKfpQYgRNLbafkgtzhmVNRsbQmqmCAUJSJWsGJWCJm4JQgAbA8JNJRcEAEfgjskavH2BXFCNeNvqUwupMUZqPtMRNHBZFe7BG6Zc6wqYgG624R3jym5dkNEeZXzYMJZH6wwCyvYpGVGyMqwE8Lb7qdMJK1VXBPfs4RkFKAxPAeSiCC6ndKrgBPJ3UoNpK79UdLfZix6LTgq5EGmzZfxaXAZSy33P89zvNhxT9VxJUNgPc8ucUEc3yhGJGAZoMqnWdAVkh8CATVB3qRLp3BoFoRV89F97N7LZcH3GjxnbtDToPsnBo7791x2eu4q9DCFesFCsWsdo8kUbruoPW98nqya9wdoNAGSvXwzjhU2R7desqGkWwoQjWiT2mG6zNrDXYkR2dq5VTCFcUZPuANF3p1Ymb8ADGM3cTvnLFSPGD6v7jWKPZhzun6FRHEGPnjmA83LVXP8zrtmtT8sKCZ5UpBx3Dw1U92ghvYEQ722bJwHSC4XaEfkXVeYbuwHAfY9E9cDRivZyQ6Zdp3PWFfeYqmRgDDNEFtu7FhuooR4eKceNkWwMbEg7bqU7WEGap9Djde9fhGKR3TjxYZD8hCKyuxmQSFPcz8wncqmqgaeNkHQtty4TuefPSp5ebuxdrqmjt3T7Goyv7f9pBwsEjkVLhLmutDZdzR5813USsf5qV4v2q1Q7LnCcJ6vTaEacGzbuuYsBcUoVZnBySXJj2UbTJt6qbShh5TAhAi1VjC7kSd4Uiye2hZFM1zbECJDJBPjbGEWXcvnAaQQnS3qDyc1JqcZWXWJxpyZQrnvB1qUVbagbGPbdHKX85nQJsdC9PqRafHxgb2vH57UMrEpMBWT9Mj9YGxGSz7L5cGyJnvdaQcm5942k6GCDFxg5TW8p8mSuhNZi5GUJCBwNiZLBfT844WtTNNTSAe63CtA3tCCnnWYxtXSqQsdxB59jVe";

    fn force_any_val<T: Arbitrary>() -> T {
        let mut runner = TestRunner::default();
        any::<T>().new_tree(&mut runner).unwrap().current()
    }

    fn create_box(box_candidate: ErgoBoxCandidate, idx: u16) -> ErgoBox {
        ErgoBox::from_box_candidate(&box_candidate, force_any_val(), idx).unwrap()
    }

    fn create_wallet_box(public_key: EcPoint, amount: u64, idx: u16) -> ErgoBox {
        let tree = Address::P2Pk(ProveDlog::new(public_key)).script().unwrap();
        let candidate = ErgoBoxCandidateBuilder::new(BoxValue::new(amount).unwrap(), tree, 0)
            .build()
            .unwrap();
        create_box(candidate, idx)
    }

    fn create_reserve(public_key: EcPoint, amount: u64, idx: u16) -> ReserveBoxSpec {
        let mut candidate = ErgoBoxCandidateBuilder::new(
            BoxValue::new(amount).unwrap(),
            AddressEncoder::new(NetworkPrefix::Mainnet)
                .parse_address_from_str(RESERVE_ADDRESS)
                .unwrap()
                .script()
                .unwrap(),
            0,
        );
        candidate.set_register_value(NonMandatoryRegisterId::R4, public_key.into());
        candidate.add_token(Token {
            token_id: TokenId::from_base64("PEXymlFlsDD9ter12B+BCPnY9QezFIfdUfSuCP4Hz0o=").unwrap(),
            amount: 1.try_into().unwrap(),
        });
        ReserveBoxSpec::try_from(&create_box(candidate.build().unwrap(), idx)).unwrap()
    }

    #[test]
    fn test_local_wallet_from_mnemonic_and_secrets() {
        let sk = DlogProverInput::random();
        let sk_hex = sk
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let config = WalletConfig {
            mnemonic: Some(
                "slow silly start wash bundle suffer bulb ancient height spin express remind today effort helmet"
                    .to_string(),
            ),
            mnemonic_password: String::new(),
            derivation_paths: vec![
                "m/44'/429'/0'/0/0".to_string(),
                "m/44'/429'/0'/0/1".to_string(),
            ],
            secrets: vec![sk_hex],
        };

        let wallet = LocalWallet::from_config(&config).unwrap().unwrap();
        assert_eq!(wallet.pubkeys().len(), 3);
        let addresses = wallet.addresses(NetworkPrefix::Mainnet);
        assert_eq!(
            addresses[0],
            "9eatpGQdYNjTi5ZZLK7Bo7C3ms6oECPnxbQTRn6sDcBNLMYSCa8"
        );
        assert_eq!(
            addresses[1],
            "9iBhwkjzUAVBkdxWvKmk7ab7nFgZRFbGpXA9gP6TAoakFnLNomk"
        );
        assert_eq!(
            wallet.get_private_key(&sk.public_image().h).unwrap().w,
            sk.w
        );
    }

    #[test]
    fn test_local_wallet_signs_reserve_transactions() {
        let sk = DlogProverInput::random();
        let pk = *sk.public_image().h.clone();
        let wallet = LocalWallet::from_secrets(vec![sk]).unwrap();
        let change_address = wallet.change_address(NetworkPrefix::Mainnet);
        let fee = *BoxValue::SAFE_USER_MIN.as_u64();
        let ctx = TxContext {
            current_height: 100,
            change_address: change_address.clone(),
            fee,
        };

        let reserve = create_reserve(pk.clone(), 1_000_000_000, 0);
        let wallet_box = create_wallet_box(pk.clone(), 2_000_000_000 + fee, 1);
        let selection = SimpleBoxSelector::new()
            .select(
                vec![wallet_box.clone()],
                (1_000_000_000 + fee).try_into().unwrap(),
                &[],
            )
            .unwrap();
        let mint_req = MintReserveRequest {
            public_key_hex: pk
                .sigma_serialize_bytes()
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            amount: 1_000_000_000,
        };
        let mint_resp = mint_reserve_transaction(
            mint_req,
            reserve.ergo_box().ergo_tree.clone(),
            selection,
            TxContext {
                current_height: 100,
                change_address: change_address.clone(),
                fee,
            },
        )
        .unwrap();

        let state_context = force_any_val::<ErgoStateContext>();
        let signed_mint = wallet
            .sign_transaction(
                mint_resp.transaction,
                vec![wallet_box.clone()],
                vec![],
                &state_context,
            )
            .unwrap();
        assert_eq!(signed_mint.inputs.len(), 1);

        let topup_resp =
            top_up_reserve_transaction(&reserve, vec![wallet_box.clone()], 1_000_000_000, &ctx)
                .unwrap();
        let signed_topup = wallet
            .sign_transaction(
                topup_resp.transaction,
                vec![wallet_box, reserve.ergo_box().clone()],
                vec![],
                &state_context,
            )
            .unwrap();
        assert_eq!(signed_topup.inputs.len(), 2);
    }
}
