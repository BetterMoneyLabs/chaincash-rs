//! Note history and methods for signing a note against reserve

use ergo_avltree_rust::{
    authenticated_tree_ops::AuthenticatedTreeOps,
    batch_avl_prover::BatchAVLProver,
    batch_node::{AVLTree, Node, NodeHeader, SerializedAdProof},
    operation::{ADKey, ADValue, KeyValue, Operation},
};
use ergo_lib::{
    ergo_chain_types::{
        blake2b256_hash,
        ec_point::{exponentiate, generator},
        Base16DecodedBytes, Digest, EcPoint,
    },
    ergotree_interpreter::sigma_protocol::{prover::ContextExtension, wscalar::Wscalar},
    ergotree_ir::{
        chain::token::TokenId,
        mir::{
            avl_tree_data::{AvlTreeData, AvlTreeFlags},
            constant::TryExtractInto,
        },
        serialization::{sigma_byte_writer::SigmaByteWriter, SigmaSerializable},
    },
};
use k256::{
    elliptic_curve::{ops::Reduce, Field, PrimeField},
    FieldBytes,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum NoteHistoryError {
    #[error("(position, reserve id) not found in history")]
    KeyNotFound,
    #[error("Attempted to insert duplicate reserve key for note")]
    DuplicateReserveKey,
    #[error("Couldn't restore ownership entry from ContextExtension")]
    InvalidContextExtension,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Signature {
    a: EcPoint,
    z: Wscalar,
}

impl Signature {
    pub const SERIALIZED_SIZE: usize = EcPoint::GROUP_SIZE + 32;
    /// Get public image of randomness
    pub fn a(&self) -> &EcPoint {
        &self.a
    }
    /// Get z (computed from randomness and private key)
    pub fn z(&self) -> &Wscalar {
        &self.z
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Signature::SERIALIZED_SIZE);
        self.write_a_bytes(&mut buf);
        self.write_z_bytes(&mut buf);
        buf
    }

    fn write_a_bytes(&self, buf: &mut Vec<u8>) {
        let mut writer = SigmaByteWriter::new(buf, None);
        self.a.sigma_serialize(&mut writer).unwrap();
    }

    fn write_z_bytes(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(self.z.as_scalar_ref().to_bytes().as_slice());
    }

    pub fn z_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.write_z_bytes(&mut buf);
        buf
    }
}

impl TryFrom<&[u8]> for Signature {
    type Error = String;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        if value.len() != Signature::SERIALIZED_SIZE {
            return Err(format!(
                "Expected {} bytes, received {}",
                Signature::SERIALIZED_SIZE,
                value.len()
            ));
        }
        let a = EcPoint::sigma_parse_bytes(&value[0..33])
            .map_err(|_| String::from("Parsing EcPoint failed"))?;
        let z_opt: Option<k256::Scalar> = k256::Scalar::from_repr(
            FieldBytes::from_exact_iter(value[33..].iter().copied())
                .ok_or_else(|| String::from("Parsing z bytes failed"))?,
        )
        .into();
        Ok(Signature {
            a,
            z: z_opt
                .ok_or_else(|| String::from("Failed to parse z"))?
                .into(),
        })
    }
}

#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
#[derive(Debug, Clone)]
pub struct OwnershipEntry {
    #[cfg_attr(
        test,
        proptest(
            strategy = "proptest::arbitrary::any_with::<TokenId>(ergo_lib::ergotree_ir::chain::token::arbitrary::ArbTokenIdParam::Arbitrary)",
            filter = "|&token_id| token_id != TokenId::from_base64(\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\").unwrap()"
        )
    )]
    pub reserve_id: TokenId,
    pub amount: u64,
    pub signature: Signature,
}

impl OwnershipEntry {
    pub fn from_context_extension(
        note_amount: u64,
        reserve_id: TokenId,
        context_extension: &ContextExtension,
    ) -> Result<Self, NoteHistoryError> {
        let a = context_extension
            .values
            .get(&1)
            .cloned()
            .ok_or(NoteHistoryError::InvalidContextExtension)?
            .try_extract_into::<EcPoint>()
            .map_err(|_| NoteHistoryError::InvalidContextExtension)?;
        let z = Wscalar::try_from(Base16DecodedBytes(
            context_extension
                .values
                .get(&2)
                .cloned()
                .ok_or(NoteHistoryError::InvalidContextExtension)?
                .try_extract_into::<Vec<u8>>()
                .map_err(|_| NoteHistoryError::InvalidContextExtension)?,
        ))
        .map_err(|_| NoteHistoryError::InvalidContextExtension)?;
        Ok(OwnershipEntry {
            reserve_id,
            amount: note_amount,
            signature: Signature { a, z },
        })
    }
}

#[derive(Clone, Default)]
pub struct NoteHistory {
    ownership_entries: Vec<OwnershipEntry>,
}

impl NoteHistory {
    pub fn new() -> Self {
        NoteHistory {
            ownership_entries: vec![],
        }
    }
    pub fn ownership_entries(&self) -> &[OwnershipEntry] {
        &self.ownership_entries
    }
    // Return prover built from signatures. Since BatchAVLProver isn't thread-safe we have to rebuild it each time
    // TODO: make BatchAVLProver thread-safe, then prover can be memoized, otherwise NoteHistory slows down significantly with more signatures
    fn prover(&self) -> Result<BatchAVLProver, NoteHistoryError> {
        build_prover(&self.ownership_entries)
    }
    /// Add a signature and generate insertion proof
    pub fn add_commitment(
        &mut self,
        commitment: OwnershipEntry,
    ) -> Result<SerializedAdProof, NoteHistoryError> {
        let mut prover = self.prover()?;
        let key = [
            &(self.ownership_entries().len() as i64).to_be_bytes()[..],
            &commitment.reserve_id.sigma_serialize_bytes().unwrap()[..],
        ]
        .concat()
        .into();
        let value = commitment.signature.serialize().into();
        let insert_op = Operation::Insert(KeyValue { key, value });
        prover
            .perform_one_operation(&insert_op)
            .map_err(|_| NoteHistoryError::DuplicateReserveKey)?;
        self.ownership_entries.push(commitment);
        Ok(prover.generate_proof())
    }
    pub fn digest(&self) -> Digest<33> {
        self.prover().unwrap().digest().unwrap()[..]
            .try_into()
            .unwrap()
    }
    pub fn lookup_proof(
        &self,
        reserve_id: TokenId,
        position: i64,
    ) -> Result<SerializedAdProof, NoteHistoryError> {
        let mut prover = self.prover()?;
        let key = [
            &(position).to_be_bytes()[..],
            &reserve_id.sigma_serialize_bytes().unwrap()[..],
        ]
        .concat()
        .into();
        prover
            .perform_one_operation(&Operation::Lookup(key))
            .transpose()
            .ok_or(NoteHistoryError::KeyNotFound)?
            .map_err(|_| NoteHistoryError::KeyNotFound)?;
        Ok(prover.generate_proof())
    }
    pub fn to_avltree(&self) -> AvlTreeData {
        let tree_flags = AvlTreeFlags::new(true, false, false);
        AvlTreeData {
            digest: self.digest(),
            tree_flags,
            key_length: 40,
            value_length_opt: None,
        }
    }
}

fn build_prover<'a>(
    signatures: impl IntoIterator<Item = &'a OwnershipEntry>,
) -> Result<BatchAVLProver, NoteHistoryError> {
    let mut prover = BatchAVLProver::new(
        AVLTree::new(
            |digest| Node::LabelOnly(NodeHeader::new(Some(*digest), None)),
            40,
            None,
        ),
        true,
    );
    signatures
        .into_iter()
        .enumerate()
        .map(
            |(
                position,
                OwnershipEntry {
                    reserve_id,
                    signature,
                    ..
                },
            )| {
                // Unwrap will only fail if OOM
                let key: ADKey = [
                    (position as i64).to_be_bytes().as_slice(),
                    reserve_id.sigma_serialize_bytes().unwrap().as_slice(),
                ]
                .concat()
                .into();
                let value: ADValue = signature.serialize().into();
                Operation::Insert(KeyValue { key, value })
            },
        )
        .try_for_each(|op| {
            prover
                .perform_one_operation(&op)
                .map_err(|_| NoteHistoryError::DuplicateReserveKey)
                .map(|_| ())
        })?;
    prover.generate_proof();
    Ok(prover)
}

pub(crate) fn sign(message: &[u8], private_key: Wscalar) -> Signature {
    let rng = rand::thread_rng();
    let private_key: k256::Scalar = private_key.into();
    let g = generator();

    let public_key = exponentiate(&g, &private_key);
    let r = k256::Scalar::random(rng);
    let a = exponentiate(&g, &r);
    let commitment = [
        &a.sigma_serialize_bytes().unwrap()[..],
        message,
        &public_key.sigma_serialize_bytes().unwrap()[..],
    ]
    .concat();
    let hash: FieldBytes = FieldBytes::clone_from_slice(&blake2b256_hash(&commitment).0);
    let e = <k256::Scalar as Reduce<k256::U256>>::reduce_bytes(&hash);

    let z = r + e * private_key;
    if z.shr_vartime(255) == k256::Scalar::ONE || e.shr_vartime(255) == k256::Scalar::ONE {
        sign(message, Wscalar::from(private_key))
    } else {
        Signature { a, z: z.into() }
    }
}

#[cfg(test)]
mod test {
    use ergo_avltree_rust::authenticated_tree_ops::AuthenticatedTreeOps;
    use ergo_avltree_rust::batch_avl_verifier::BatchAVLVerifier;
    use ergo_avltree_rust::batch_node::{AVLTree, Node, NodeHeader};
    use ergo_avltree_rust::operation::{KeyValue, Operation};
    use ergo_lib::ergotree_ir::serialization::SigmaSerializable;
    use proptest::arbitrary::any;
    use proptest::collection::vec;
    use proptest::proptest;

    use crate::note_history::{build_prover, NoteHistory, OwnershipEntry, Signature};
    proptest! {
        #[test]
        fn test_signature_roundtrip(signature in any::<Signature>()) {
            let serialized = signature.serialize();
            assert_eq!(Signature::try_from(&serialized[..]).unwrap(), signature);
        }
        #[test]
        fn test_prover_verifier(commitments in vec(any::<OwnershipEntry>(), 0..50)) {
            let prover = build_prover(commitments.iter()).unwrap();
            let mut note_history = NoteHistory::new();
            for (i, commitment) in commitments.into_iter().enumerate() {
                let digest = note_history.digest();
                let proof = note_history.add_commitment(commitment.clone()).unwrap();
                let mut bv = BatchAVLVerifier::new(
                            &digest.0.to_vec().into(),
                            &proof,
                            AVLTree::new(
                                |digest| Node::LabelOnly(NodeHeader::new(Some(*digest), None)),
                                40,
                                None,
                            ),
                            None,
                            None,
                        )
                        .unwrap();
                bv.perform_one_operation(&Operation::Insert(KeyValue { key: [(i as i64).to_be_bytes().as_slice(), commitment.reserve_id.sigma_serialize_bytes().unwrap().as_slice()].concat().into(), value: commitment.signature.serialize().into() })).unwrap();
            }
            assert_eq!(&note_history.digest().0[..], &prover.digest().unwrap()[..]);
        }
    }
}
