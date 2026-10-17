use crate::boxes::ReserveBoxSpec;

use super::{TransactionError, TxContext};
use ergo_lib::chain::ergo_box::box_builder::ErgoBoxCandidateBuilder;
use ergo_lib::chain::transaction::ergo_transaction::ErgoTransaction;
use ergo_lib::chain::transaction::unsigned::UnsignedTransaction;
use ergo_lib::chain::transaction::Transaction;
use ergo_lib::ergo_chain_types::EcPoint;
use ergo_lib::ergotree_interpreter::sigma_protocol::prover::ContextExtension;
use ergo_lib::ergotree_ir::chain::address::NetworkAddress;
use ergo_lib::ergotree_ir::chain::ergo_box::{ErgoBox, ErgoBoxCandidate};
use ergo_lib::ergotree_ir::chain::token::TokenAmount;
use ergo_lib::ergotree_ir::chain::{ergo_box::NonMandatoryRegisterId, token::Token};
use ergo_lib::ergotree_ir::ergo_tree::ErgoTree;
use ergo_lib::wallet::box_selector::{BoxSelector, SimpleBoxSelector};
use ergo_lib::wallet::{box_selector::BoxSelection, tx_builder::TxBuilder};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, utoipa::ToSchema)]
pub struct MintReserveRequest {
    pub public_key_hex: String,
    pub amount: u64,
}

pub struct ReserveResponse<T: ErgoTransaction> {
    /// Reserve Box
    pub reserve_box: ReserveBoxSpec,
    /// Unsigned transaction that creates reserve box and mints reserve NFT
    pub transaction: T,
}

pub type SignedReserveResponse = ReserveResponse<Transaction>;

pub fn mint_reserve_transaction(
    request: MintReserveRequest,
    reserve_tree: ErgoTree,
    inputs: BoxSelection<ErgoBox>,
    context: TxContext,
) -> Result<ReserveResponse<UnsignedTransaction>, TransactionError> {
    let pk = EcPoint::try_from(request.public_key_hex).map_err(TransactionError::Parsing)?;
    let mut reserve_box_builder = ErgoBoxCandidateBuilder::new(
        request.amount.try_into()?,
        reserve_tree,
        context.current_height,
    );
    let nft_id = inputs
        .boxes
        .get(0)
        .ok_or_else(|| {
            TransactionError::MissingBox(
                "mint_reserve_transaction: failed to find input box required to mint nft"
                    .to_string(),
            )
        })?
        .box_id();
    let nft = Token {
        token_id: nft_id.into(),
        amount: 1.try_into()?,
    };
    reserve_box_builder.add_token(nft);
    reserve_box_builder.set_register_value(NonMandatoryRegisterId::R4, pk.into());

    let unsigned_transaction = TxBuilder::new(
        inputs,
        vec![reserve_box_builder.build()?],
        context.current_height,
        context.fee.try_into()?,
        NetworkAddress::try_from(context.change_address)?.address(),
    )
    .build()?;

    Ok(ReserveResponse {
        // Unwrap is safe here since transaction layout is fixed (reserve box at output #0)
        reserve_box: unsigned_transaction
            .outputs()
            .first()
            .unwrap()
            .try_into()
            .unwrap(),
        transaction: unsigned_transaction,
    })
}

pub fn top_up_reserve_transaction(
    reserve: &ReserveBoxSpec,
    mut wallet_boxes: Vec<ErgoBox>,
    top_up_amount: u64,
    context: &TxContext,
) -> Result<ReserveResponse<UnsignedTransaction>, TransactionError> {
    if top_up_amount < 1_000_000_000 {
        return Err(TransactionError::TopUpAmountError(top_up_amount));
    }
    wallet_boxes.push(reserve.ergo_box().clone());
    let box_selector = SimpleBoxSelector::new();
    let box_selection = box_selector.select(
        wallet_boxes,
        (reserve.ergo_box().value.as_u64() + top_up_amount + context.fee).try_into()?,
        &[Token {
            token_id: reserve.identifier,
            amount: TokenAmount::try_from(1).unwrap(),
        }],
    )?;
    let mut reserve_box_candidate: ErgoBoxCandidate = reserve.ergo_box().clone().into();
    reserve_box_candidate.value = reserve_box_candidate
        .value
        .checked_add(&top_up_amount.try_into()?)?;
    reserve_box_candidate.creation_height = context.current_height;
    let output_candidates = vec![reserve_box_candidate];
    let mut tx_builder = TxBuilder::new(
        box_selection,
        output_candidates,
        context.current_height,
        context.fee.try_into()?,
        NetworkAddress::try_from(context.change_address.clone())?.address(),
    );
    let mut context_extension = ContextExtension::empty();
    // 10 for top-up action. 1 = top-up, 0 = output index of new reserve box
    context_extension.values.insert(0u8, 10i8.into());
    tx_builder.set_context_extension(reserve.box_id(), context_extension);
    let transaction = tx_builder.build()?;
    let reserve_box = ReserveBoxSpec::try_from(transaction.outputs().first().unwrap()).unwrap();
    Ok(ReserveResponse {
        reserve_box,
        transaction,
    })
}

#[cfg(test)]
mod test {
    use ergo_lib::{
        ergotree_interpreter::sigma_protocol::private_input::DlogProverInput,
        ergotree_ir::chain::{
            address::{Address, NetworkAddress, NetworkPrefix},
            ergo_box::box_value::BoxValue,
        },
        wallet::{signing::TransactionContext, Wallet},
    };

    use crate::{
        test_util::{create_reserve, create_wallet_box, force_any_val},
        transactions::TxContext,
    };

    use super::top_up_reserve_transaction;

    #[test]
    fn test_topup() {
        let top_up_amount = 1_000_000_000;
        let private_key = DlogProverInput::random();
        let public_key = private_key.public_image().h;
        let reserve = create_reserve(*public_key.clone(), 1_000_000_000);
        let context = TxContext {
            current_height: 0,
            change_address: NetworkAddress::new(
                NetworkPrefix::Mainnet,
                &Address::P2Pk(private_key.public_image()),
            )
            .to_base58(),
            fee: *BoxValue::SAFE_USER_MIN.as_u64(),
        };
        let mut wallet_boxes = vec![create_wallet_box(
            *public_key.clone(),
            top_up_amount + context.fee,
        )];
        let reserve_response =
            top_up_reserve_transaction(&reserve, wallet_boxes.clone(), top_up_amount, &context)
                .unwrap();
        wallet_boxes.push(reserve.ergo_box().clone());
        let tx_context =
            TransactionContext::new(reserve_response.transaction, wallet_boxes, vec![]).unwrap();
        let wallet = Wallet::from_secrets(vec![private_key.into()]);
        let transaction = wallet
            .sign_transaction(tx_context, &force_any_val(), None)
            .unwrap();
        let reserve_output = transaction.outputs.first();
        assert_eq!(
            *reserve_output.value.as_u64(),
            reserve.ergo_box().value.as_u64() + top_up_amount
        );
    }
}
