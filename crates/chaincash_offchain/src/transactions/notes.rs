use crate::boxes::{Note, ReserveBoxSpec};
use crate::note_history::NoteHistory;

use super::{TransactionError, TxContext};
use ergo_avltree_rust::authenticated_tree_ops::AuthenticatedTreeOps;
use ergo_avltree_rust::batch_avl_prover::BatchAVLProver;
use ergo_avltree_rust::batch_node::{AVLTree, Node, NodeHeader};
use ergo_lib::chain::ergo_box::box_builder::ErgoBoxCandidateBuilder;
use ergo_lib::chain::transaction::ergo_transaction::ErgoTransaction;
use ergo_lib::chain::transaction::unsigned::UnsignedTransaction;
use ergo_lib::chain::transaction::{DataInput, Transaction};
use ergo_lib::ergo_chain_types::{ADDigest, EcPoint};
use ergo_lib::ergotree_interpreter::sigma_protocol::prover::ContextExtension;
use ergo_lib::ergotree_interpreter::sigma_protocol::wscalar::Wscalar;
use ergo_lib::ergotree_ir::chain::address::NetworkAddress;
use ergo_lib::ergotree_ir::chain::ergo_box::box_value::{BoxValue, BoxValueError};
use ergo_lib::ergotree_ir::chain::ergo_box::{ErgoBoxCandidate, NonMandatoryRegisterId};
use ergo_lib::ergotree_ir::chain::{ergo_box::ErgoBox, token::Token};
use ergo_lib::ergotree_ir::ergo_tree::ErgoTree;
use ergo_lib::ergotree_ir::mir::avl_tree_data::{AvlTreeData, AvlTreeFlags};
use ergo_lib::ergotree_ir::mir::constant::TryExtractInto;
use ergo_lib::wallet::box_selector::{
    BoxSelection, BoxSelector, ErgoBoxAssetsData, SimpleBoxSelector,
};
use ergo_lib::wallet::tx_builder::TxBuilder;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, utoipa::ToSchema)]
pub struct MintNoteRequest {
    pub owner_public_key_hex: String,
    // Currently the amount in mg of gold
    // later will refactor to support more nominations
    // this is represented by a token at index 0 on the box
    pub gold_amount_mg: u64,
}

pub struct MintNoteResponse<T: ErgoTransaction> {
    pub note: Note,
    pub transaction: T,
}

pub type SignedMintNoteResponse = MintNoteResponse<Transaction>;

pub fn mint_note_transaction(
    request: MintNoteRequest,
    note_tree: ErgoTree,
    inputs: BoxSelection<ErgoBox>,
    context: TxContext,
) -> Result<MintNoteResponse<UnsignedTransaction>, TransactionError> {
    let owner_pk =
        EcPoint::try_from(request.owner_public_key_hex).map_err(TransactionError::Parsing)?;
    let prover = BatchAVLProver::new(
        AVLTree::new(
            |digest| Node::LabelOnly(NodeHeader::new(Some(*digest), None)),
            40,
            None,
        ),
        true,
    );
    let digest: ADDigest = prover
        .digest()
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let avl_flags = AvlTreeFlags::new(true, false, false);
    let avl_tree = AvlTreeData {
        digest,
        tree_flags: avl_flags,
        key_length: 40,
        value_length_opt: None,
    };
    let token_id = inputs
        .boxes
        .get(0)
        .ok_or_else(|| {
            TransactionError::MissingBox(
                "failed to find input box required to mint nft".to_string(),
            )
        })?
        .box_id();
    let token = Token {
        token_id: token_id.into(),
        amount: request.gold_amount_mg.try_into()?,
    };
    let mut note_box_builder =
        ErgoBoxCandidateBuilder::new(BoxValue::SAFE_USER_MIN, note_tree, context.current_height);
    note_box_builder.add_token(token);
    note_box_builder.set_register_value(NonMandatoryRegisterId::R4, avl_tree.into());
    note_box_builder.set_register_value(NonMandatoryRegisterId::R5, owner_pk.into());
    note_box_builder.set_register_value(NonMandatoryRegisterId::R6, 0i64.into());
    let unsigned_transaction = TxBuilder::new(
        inputs,
        vec![note_box_builder.build()?],
        context.current_height,
        context.fee.try_into()?,
        NetworkAddress::try_from(context.change_address)?.address(),
    )
    .build()?;
    let note = Note::new(
        unsigned_transaction.outputs().first().unwrap().clone(),
        NoteHistory::new(),
    )
    .unwrap();
    Ok(MintNoteResponse {
        note,
        transaction: unsigned_transaction,
    })
}

fn create_note_candidate(
    note: &Note,
    new_digest: AvlTreeData,
    recipient: EcPoint,
    token_amount: u64,
    height: u32,
) -> Result<ErgoBoxCandidate, TransactionError> {
    // Note value must be >= old note's value
    let mut box_candidate = ErgoBoxCandidateBuilder::new(
        note.ergo_box().value,
        note.ergo_box().ergo_tree.clone(),
        height,
    );
    box_candidate.add_token(Token {
        token_id: note.note_id,
        amount: token_amount.try_into()?,
    });
    box_candidate.set_register_value(NonMandatoryRegisterId::R4, new_digest.into());
    box_candidate.set_register_value(NonMandatoryRegisterId::R5, recipient.into());
    box_candidate.set_register_value(NonMandatoryRegisterId::R6, (note.length as i64 + 1).into());
    Ok(box_candidate.build()?)
}

pub struct SpendNoteResponse<T: ErgoTransaction> {
    pub transaction: T,
    pub recipient_note: Note,
    pub change_note: Option<Note>,
}

pub type SignedSpendNoteResponse = SpendNoteResponse<Transaction>;

pub fn spend_note_transaction(
    note: &Note,
    reserve: &ReserveBoxSpec,
    private_key: Wscalar,
    recipient: EcPoint,
    amount: u64,
    wallet_boxes: Vec<ErgoBox>,
    context: &TxContext,
) -> Result<SpendNoteResponse<UnsignedTransaction>, TransactionError> {
    let change_amount =
        note.amount
            .as_u64()
            .checked_sub(amount)
            .ok_or(TransactionError::NoteAmountError {
                input_amount: *note.amount.as_u64(),
                output_amount: amount,
            })?;
    let has_change = change_amount > 0;

    // If there is a change note box must have atleast as many erg as the original note
    let erg_needed = note.ergo_box().value.as_u64() * has_change as u64 + context.fee;
    let box_selector = SimpleBoxSelector::new();
    let BoxSelection {
        boxes,
        change_boxes,
    } = box_selector.select(wallet_boxes, BoxValue::new(erg_needed)?, &[])?;
    let mut boxes = boxes.to_vec();
    boxes.push(note.ergo_box().clone());

    let mut new_history = note.history.clone();
    let signature = note.sign_note(reserve.identifier, private_key)?;
    let proof = new_history.add_commitment(signature.clone())?;
    let new_digest = new_history.to_avltree();

    let box_selection = BoxSelection {
        boxes: boxes.try_into().unwrap(),
        change_boxes,
    };
    let mut output_candidates = vec![create_note_candidate(
        note,
        new_digest.clone(),
        recipient,
        amount,
        context.current_height,
    )?];
    if has_change {
        output_candidates.push(create_note_candidate(
            note,
            new_digest,
            note.owner.clone(), // TODO: allow setting change address in request
            change_amount,
            context.current_height,
        )?)
    }
    let mut tx_builder = TxBuilder::new(
        box_selection,
        output_candidates,
        context.current_height,
        context.fee.try_into()?,
        NetworkAddress::try_from(context.change_address.clone())?.address(),
    );
    let mut context_extension = ContextExtension::empty();
    context_extension.values.insert(0, 0i8.into()); // note output
    context_extension
        .values
        .insert(1, signature.signature.a().clone().into()); // signature a
    context_extension
        .values
        .insert(2, signature.signature.z_bytes().into()); // signature z
    context_extension.values.insert(3, proof.to_vec().into());
    if has_change {
        context_extension.values.insert(4, 1i8.into()); // change output index
    }
    tx_builder.set_context_extension(note.ergo_box().box_id(), context_extension);
    tx_builder.set_data_inputs(vec![DataInput {
        box_id: reserve.ergo_box().box_id(),
    }]);
    let transaction = tx_builder.build()?;

    let recipient_note = Note::new(
        transaction.outputs().first().unwrap().clone(),
        new_history.clone(),
    )
    .unwrap();
    let change_note = has_change
        .then(|| Note::new(transaction.outputs().get(1).unwrap().clone(), new_history).unwrap());
    Ok(SpendNoteResponse {
        transaction,
        recipient_note,
        change_note,
    })
}

fn create_receipt_candidate(
    note: &Note,
    reserve: &ReserveBoxSpec,
    position: i64,
    receipt_contract: &ErgoTree,
    height: u32,
) -> Result<ErgoBoxCandidate, TransactionError> {
    // Note value must be >= old note's value
    let mut box_candidate =
        ErgoBoxCandidateBuilder::new(note.ergo_box().value, receipt_contract.clone(), height);
    box_candidate.add_token(Token {
        token_id: note.note_id,
        amount: note.amount,
    });
    box_candidate.set_register_value(NonMandatoryRegisterId::R4, note.history.to_avltree().into());
    box_candidate.set_register_value(NonMandatoryRegisterId::R5, position.into());
    box_candidate.set_register_value(NonMandatoryRegisterId::R6, (height as i32).into());
    box_candidate.set_register_value(NonMandatoryRegisterId::R7, reserve.owner.clone().into());
    Ok(box_candidate.build()?)
}

pub fn redeem_note(
    note_box: &Note,
    reserve_box: &ReserveBoxSpec,
    oracle_box: &ErgoBox,
    buyback_box: &ErgoBox,
    receipt_contract: &ErgoTree,
    wallet_boxes: Vec<ErgoBox>,
    context: &TxContext,
) -> Result<UnsignedTransaction, TransactionError> {
    // TX structure:
    // INPUTS: [note, reserve, buyback, wallet boxes...]
    // OUTPUTS: [reserve, receipt, buyback, change]
    // DATAINPUTS: [oracle]
    // nano erg per mg of gold
    let price: i64 = oracle_box
        .get_register(NonMandatoryRegisterId::R4.into())
        .unwrap()
        .unwrap()
        .try_extract_into::<i64>()
        .unwrap()
        / 1_000_000;
    let redeemable = std::cmp::min(
        *reserve_box.ergo_box().value.as_u64() - BoxValue::SAFE_USER_MIN.as_u64(),
        (note_box.amount.as_u64() * price as u64 * 98) / 100,
    );
    let to_oracle = (redeemable * 2) / 1000;
    let to_change = redeemable - to_oracle;

    let (position, max_amount) = note_box
        .history
        .ownership_entries()
        .iter()
        .enumerate()
        .rev()
        .find(|(_, entry)| entry.reserve_id == reserve_box.identifier)
        .map(|(position, entry)| (position as i64, entry.amount))
        .ok_or(TransactionError::ReserveEntryNotFound(
            reserve_box.identifier,
        ))?;
    let BoxSelection {
        boxes,
        mut change_boxes,
    } = SimpleBoxSelector::new().select(wallet_boxes, context.fee.try_into().unwrap(), &[])?;
    let boxes = [
        &[
            note_box.ergo_box().clone(),
            reserve_box.ergo_box().clone(),
            buyback_box.clone(),
        ][..],
        boxes.as_slice(),
    ]
    .concat();
    if let Some(change_box) = change_boxes.first_mut() {
        change_box.value = BoxValue::new(change_box.value.as_u64() + to_change)?;
    } else {
        change_boxes.push(ErgoBoxAssetsData {
            value: BoxValue::new(to_change)?,
            tokens: None,
        })
    }
    let mut reserve_output = ErgoBoxCandidate::from(reserve_box.ergo_box().clone());
    reserve_output.creation_height = context.current_height;
    reserve_output.value = BoxValue::new(
        reserve_output
            .value
            .as_u64()
            .checked_sub(redeemable)
            .ok_or(BoxValueError::Overflow)?,
    )?;
    let receipt_output = create_receipt_candidate(
        note_box,
        reserve_box,
        position,
        receipt_contract,
        context.current_height,
    )?;
    let mut buyback_output = ErgoBoxCandidate::from(buyback_box.clone());
    buyback_output.creation_height = context.current_height;
    buyback_output.value = BoxValue::new(buyback_output.value.as_u64() + to_oracle)?;
    let mut tx_builder = TxBuilder::new(
        BoxSelection {
            boxes: boxes.try_into().unwrap(),
            change_boxes,
        },
        vec![reserve_output, receipt_output, buyback_output],
        context.current_height,
        context.fee.try_into().unwrap(),
        NetworkAddress::try_from(context.change_address.clone())?.address(),
    );
    tx_builder.set_data_inputs(vec![DataInput::from(oracle_box.box_id())]);
    tx_builder.set_context_extension(reserve_box.box_id(), {
        let mut context_extension = ContextExtension::empty();
        context_extension.values.insert(0, 0i8.into());
        context_extension.values.insert(
            1,
            note_box
                .history
                .lookup_proof(reserve_box.identifier, position)?
                .to_vec()
                .into(),
        );
        context_extension
            .values
            .insert(2, max_amount.to_be_bytes().to_vec().into());
        context_extension.values.insert(3, position.into());
        context_extension.values.insert(4, false.into());
        context_extension
    });
    tx_builder.set_context_extension(note_box.ergo_box().box_id(), {
        let mut context_extension = ContextExtension::empty();
        context_extension.values.insert(0, (-1i8).into());
        context_extension
    });
    tx_builder.set_context_extension(buyback_box.box_id(), {
        let mut context_extension = ContextExtension::empty();
        context_extension.values.insert(0, (1i32).into());
        context_extension
    });
    Ok(tx_builder.build()?)
}

#[cfg(test)]
mod test {
    use ergo_lib::{
        chain::ergo_state_context::ErgoStateContext,
        ergotree_interpreter::sigma_protocol::private_input::DlogProverInput,
        ergotree_ir::chain::{
            address::{Address, AddressEncoder, NetworkAddress, NetworkPrefix},
            ergo_box::box_value::BoxValue,
        },
        wallet::{secret_key::SecretKey, signing::TransactionContext, Wallet},
    };

    use crate::{
        test_util::{
            create_buyback_box, create_note, create_oracle_box, create_reserve, create_wallet_box,
            force_any_val, RECEIPT_ADDRESS,
        },
        transactions::TxContext,
    };

    use super::{redeem_note, spend_note_transaction, SpendNoteResponse};
    // Test spending a note with change output
    #[test]
    fn test_spend_note() {
        let private_key = DlogProverInput::random();
        let public_key = private_key.public_image().h;
        let note = create_note(&public_key, 10);
        let reserve = create_reserve(*public_key.clone(), 1_000_000_000);

        let mut wallet_boxes = vec![create_wallet_box(*public_key.clone(), 1_000_000_000)];
        let recipient = DlogProverInput::random().public_image().h;
        let note_response = spend_note_transaction(
            &note,
            &reserve,
            private_key.w.clone(),
            *recipient,
            8,
            wallet_boxes.clone(),
            &TxContext {
                current_height: 0,
                change_address: NetworkAddress::new(
                    NetworkPrefix::Mainnet,
                    &Address::P2Pk(private_key.public_image()),
                )
                .to_base58(),
                fee: *BoxValue::SAFE_USER_MIN.as_u64(),
            },
        )
        .unwrap();
        wallet_boxes.push(note.ergo_box().clone());
        let tx_context = TransactionContext::new(
            note_response.transaction,
            wallet_boxes,
            vec![reserve.ergo_box().clone()],
        )
        .unwrap();
        let wallet = Wallet::from_secrets(vec![private_key.into()]);
        let transaction = wallet
            .sign_transaction(tx_context, &force_any_val(), None)
            .unwrap();
        let note_output_tokens = transaction.outputs.get(0).unwrap().tokens.as_ref().unwrap();
        let change_output_tokens = transaction.outputs.get(1).unwrap().tokens.as_ref().unwrap();
        assert_eq!(*(note_output_tokens.get(0).unwrap().amount.as_u64()), 8);
        assert_eq!(*(change_output_tokens.get(0).unwrap().amount.as_u64()), 2);
    }
    #[test]
    fn test_redeem_note() {
        const NANOERG_PER_KG: u64 = 1_000_000_000;
        let oracle_box = create_oracle_box(NANOERG_PER_KG as i64);
        let buyback_box = create_buyback_box();
        let reserve_owner_sk = DlogProverInput::random();
        let reserve_owner_pk = *reserve_owner_sk.public_image().h.clone();
        let wallet_boxes = vec![create_wallet_box(reserve_owner_pk.clone(), 1_000_000_000)];
        let note = create_note(&reserve_owner_pk, 1000);
        let reserve = create_reserve(reserve_owner_pk.clone(), 1_000_000_000);

        let state_context = force_any_val::<ErgoStateContext>();
        let context = TxContext {
            current_height: state_context.pre_header.height,
            change_address: NetworkAddress::new(
                NetworkPrefix::Mainnet,
                &Address::P2Pk(reserve_owner_sk.public_image()),
            )
            .to_base58(),
            fee: *BoxValue::SAFE_USER_MIN.as_u64(),
        };
        let recipient_sk = DlogProverInput::random();
        let recipient_pk = *recipient_sk.public_image().h.clone();
        let SpendNoteResponse {
            transaction: _,
            recipient_note,
            change_note: _,
        } = spend_note_transaction(
            &note,
            &reserve,
            reserve_owner_sk.w.clone(),
            recipient_pk.clone(),
            1000,
            wallet_boxes,
            &context,
        )
        .unwrap();

        let wallet_boxes = vec![create_wallet_box(recipient_pk, 1_000_000_000)];
        let receipt_tree = AddressEncoder::new(NetworkPrefix::Mainnet)
            .parse_address_from_str(RECEIPT_ADDRESS)
            .unwrap()
            .script()
            .unwrap();
        let tx = redeem_note(
            &recipient_note,
            &reserve,
            &oracle_box,
            &buyback_box,
            &receipt_tree,
            wallet_boxes.clone(),
            &context,
        )
        .unwrap();
        assert_eq!(
            *tx.output_candidates
                .get(tx.output_candidates.len() - 2)
                .unwrap()
                .value
                .as_u64(),
            wallet_boxes[0].value.as_u64() - context.fee
                + (((NANOERG_PER_KG / 1_000_000) * note.amount.as_u64() * 98) / 100) * 998 / 1000,
        );
        let mut input_boxes = wallet_boxes.clone();
        input_boxes.extend_from_slice(&[
            recipient_note.ergo_box().clone(),
            buyback_box.clone(),
            reserve.ergo_box().clone(),
        ]);
        let wallet = Wallet::from_secrets(vec![SecretKey::DlogSecretKey(recipient_sk)]);
        wallet
            .sign_transaction(
                TransactionContext::new(tx, input_boxes.clone(), vec![oracle_box.clone()]).unwrap(),
                &state_context,
                None,
            )
            .unwrap();
        // Test redemption against undercollateralized reserve. In this case we can only redeem reserve.value - minimum amount of ERG needed for new box
        let reserve = create_reserve(reserve_owner_pk, BoxValue::SAFE_USER_MIN.as_u64() + 1000);

        let tx = redeem_note(
            &recipient_note,
            &reserve,
            &oracle_box,
            &buyback_box,
            &receipt_tree,
            wallet_boxes.clone(),
            &context,
        )
        .unwrap();
        let mut input_boxes = wallet_boxes.clone();
        input_boxes.extend_from_slice(&[
            recipient_note.ergo_box().clone(),
            buyback_box.clone(),
            reserve.ergo_box().clone(),
        ]);
        assert_eq!(
            *tx.output_candidates
                .get(tx.output_candidates.len() - 2)
                .unwrap()
                .value
                .as_u64(),
            wallet_boxes[0].value.as_u64() - context.fee + 998
        );
        wallet
            .sign_transaction(
                TransactionContext::new(tx, input_boxes.clone(), vec![oracle_box.clone()]).unwrap(),
                &state_context,
                None,
            )
            .unwrap();
    }
}
