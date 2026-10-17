use crate::note_history::{sign, NoteHistory, OwnershipEntry};
use ergo_lib::{
    ergo_chain_types::{
        ec_point::{exponentiate, generator},
        ADDigest, EcPoint,
    },
    ergotree_interpreter::sigma_protocol::wscalar::Wscalar,
    ergotree_ir::{
        chain::{
            ergo_box::{BoxId, ErgoBox, NonMandatoryRegisterId, RegisterValueError},
            token::{Token, TokenAmount, TokenId},
        },
        mir::{avl_tree_data::AvlTreeData, constant::TryExtractInto},
        serialization::SigmaSerializable,
        types::stype::SType,
    },
};
use serde::Serialize;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to access register")]
    BadRegister(#[from] RegisterValueError),

    #[error("Box field was unexpectedly empty: {0}")]
    FieldNotSet(&'static str),

    #[error("Box field '{field}' was set to incorrect type: {tpe}")]
    InvalidType { field: String, tpe: SType },
    #[error("Box field '{field}' was not in bounds")]
    InvalidField { field: &'static str },

    #[error("AVL digest for box could not be rebuilt from note history. note history digest: {history_digest}, box digest: {box_digest}")]
    InvalidAVLDigest {
        box_digest: ADDigest,
        history_digest: ADDigest,
    },
    #[error("Chain length in register R6 does not match, expected {expected}, found {found}")]
    InvalidChainLength { expected: u64, found: u64 },
    #[error(
        "Private key provided derives to public key {found_pubkey}, expected {expected_pubkey}"
    )]
    InvalidPrivateKey {
        expected_pubkey: String,
        found_pubkey: String,
    },
}

pub struct Note {
    pub owner: EcPoint,
    pub history: NoteHistory,
    pub length: u64,
    pub note_id: TokenId,
    pub amount: TokenAmount,
    inner: ErgoBox,
}

impl Note {
    pub fn new(note_box: ErgoBox, history: NoteHistory) -> Result<Self, Error> {
        let owner = note_box
            .get_register(NonMandatoryRegisterId::R5.into())?
            .ok_or(Error::FieldNotSet("owner"))
            .and_then(|reg| {
                if reg.tpe == SType::SGroupElement {
                    Ok(reg.v.try_extract_into::<EcPoint>().unwrap())
                } else {
                    Err(Error::InvalidType {
                        field: "owner".to_owned(),
                        tpe: reg.tpe,
                    })
                }
            })?;
        let chain_length: u64 = note_box
            .get_register(NonMandatoryRegisterId::R6.into())?
            .ok_or(Error::FieldNotSet("chain length"))
            .and_then(|reg| {
                if reg.tpe == SType::SLong {
                    Ok(reg.v.try_extract_into::<i64>().unwrap())
                } else {
                    Err(Error::InvalidType {
                        field: "chain length".to_owned(),
                        tpe: reg.tpe,
                    })
                }
            })?
            .try_into()
            .map_err(|_| Error::InvalidField {
                field: "chain length",
            })?;
        if chain_length != history.ownership_entries().len() as u64 {
            return Err(Error::InvalidChainLength {
                expected: chain_length,
                found: history.ownership_entries().len() as u64,
            });
        }
        let box_avltree = note_box
            .get_register(NonMandatoryRegisterId::R4.into())?
            .ok_or(Error::FieldNotSet("avl tree"))
            .and_then(|reg| {
                if reg.tpe == SType::SAvlTree {
                    Ok(reg.v.try_extract_into::<AvlTreeData>().unwrap())
                } else {
                    Err(Error::InvalidType {
                        field: "avl tree".to_owned(),
                        tpe: reg.tpe,
                    })
                }
            })?;
        if box_avltree.digest != history.digest() {
            return Err(Error::InvalidAVLDigest {
                box_digest: box_avltree.digest,
                history_digest: history.digest(),
            });
        }

        let Token { token_id, amount } = note_box
            .tokens
            .as_ref()
            .ok_or(Error::FieldNotSet("note box missing NFT"))?
            .get(0)
            .ok_or(Error::FieldNotSet(
                "token at index 0 missing, no identifier nft",
            ))?
            .clone();

        Ok(Note {
            owner,
            history,
            length: chain_length,
            note_id: token_id,
            amount,
            inner: note_box,
        })
    }

    fn bytes_to_sign(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(48);
        buf.extend_from_slice(&self.length.to_be_bytes());
        buf.extend_from_slice(&self.amount.as_u64().to_be_bytes());
        buf.extend_from_slice(&self.note_id.sigma_serialize_bytes().unwrap());
        buf
    }

    // Sign a note against reserve id, returning a new ownership entry
    // TODO: consider passing ReserveBoxSpec instead
    pub(crate) fn sign_note(
        &self,
        reserve_id: TokenId,
        private_key: Wscalar,
    ) -> Result<OwnershipEntry, Error> {
        let public_key = exponentiate(&generator(), private_key.as_scalar_ref());
        if public_key != self.owner {
            return Err(Error::InvalidPrivateKey {
                expected_pubkey: self.owner.to_string(),
                found_pubkey: public_key.to_string(),
            });
        }
        let bytes_to_sign = self.bytes_to_sign();
        let signature = sign(&bytes_to_sign, private_key);
        Ok(OwnershipEntry {
            reserve_id,
            amount: self.amount.into(),
            signature,
        })
    }
    pub fn ergo_box(&self) -> &ErgoBox {
        &self.inner
    }
}

#[derive(Serialize)]
pub struct ReserveBoxSpec {
    pub owner: EcPoint,
    pub refund_height: Option<i64>,
    pub identifier: TokenId,
    #[serde(skip)]
    inner: ErgoBox,
}

impl ReserveBoxSpec {
    pub fn box_id(&self) -> BoxId {
        self.inner.box_id()
    }
    pub fn ergo_box(&self) -> &ErgoBox {
        &self.inner
    }
}

impl TryFrom<&ErgoBox> for ReserveBoxSpec {
    type Error = Error;

    fn try_from(value: &ErgoBox) -> Result<Self, Self::Error> {
        let owner = value
            .get_register(NonMandatoryRegisterId::R4.into())?
            .ok_or(Error::FieldNotSet("owner"))
            .and_then(|reg| {
                if reg.tpe == SType::SGroupElement {
                    Ok(reg.v.try_extract_into::<EcPoint>().unwrap())
                } else {
                    Err(Error::InvalidType {
                        field: "owner".to_owned(),
                        tpe: reg.tpe,
                    })
                }
            })?;
        let refund_height = value
            .get_register(NonMandatoryRegisterId::R5.into())?
            .map(|reg| {
                if reg.tpe == SType::SLong {
                    Ok(reg.v.try_extract_into::<i64>().unwrap())
                } else {
                    Err(Error::InvalidType {
                        field: "refund_height".to_owned(),
                        tpe: reg.tpe,
                    })
                }
            })
            .transpose()?;
        let identifier = value
            .tokens
            .as_ref()
            .ok_or(Error::FieldNotSet("reserve box missing NFT"))?
            .get(0)
            .ok_or(Error::FieldNotSet(
                "token at index 0 missing, no identifier nft",
            ))?
            .token_id;

        Ok(Self {
            owner,
            refund_height,
            identifier,
            inner: value.clone(),
        })
    }
}
