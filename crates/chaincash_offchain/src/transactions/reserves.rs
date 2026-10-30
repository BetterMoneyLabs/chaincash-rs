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

/// Number of blocks required between refund initiation (`R5`) and refund completion
/// (`refundNotificationPeriod = 14400` in `contracts/onchain/reserve.es`, ~20 days).
pub const REFUND_TIMELOCK_BLOCKS: u32 = 14_400;

const ACTION_TOP_UP: i8 = 1;
const ACTION_INIT_REFUND: i8 = 2;
const ACTION_CANCEL_REFUND: i8 = 3;
const ACTION_COMPLETE_REFUND: i8 = 4;

/// Computes the block height at or after which a refund initiated at `init_height` unlocks.
pub fn calculate_refund_unlock_height(init_height: i32) -> u32 {
    (i64::from(init_height) + i64::from(REFUND_TIMELOCK_BLOCKS)).max(0) as u32
}

#[derive(Serialize, Deserialize, Clone)]
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

pub struct CompletedRefundUnsigned {
    /// Updated reserve output box with `R5`/`R6` cleared and remaining balance
    pub reserve_box: ReserveBoxSpec,
    /// Unsigned refund completion transaction
    pub transaction: UnsignedTransaction,
    /// Actual nanoERG amount withdrawn from the reserve to the wallet change address
    pub refunded_amount: u64,
}

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
    context_extension
        .values
        .insert(0u8, (ACTION_TOP_UP * 10).into());
    tx_builder.set_context_extension(reserve.box_id(), context_extension);
    let transaction = tx_builder.build()?;
    let reserve_box = ReserveBoxSpec::try_from(transaction.outputs().first().unwrap()).unwrap();
    Ok(ReserveResponse {
        reserve_box,
        transaction,
    })
}

fn build_reserve_action_candidate(
    reserve: &ReserveBoxSpec,
    output_value: u64,
    refund_registers: Option<(i32, i64)>,
    current_height: u32,
) -> Result<ErgoBoxCandidate, TransactionError> {
    let mut builder = ErgoBoxCandidateBuilder::new(
        output_value.try_into()?,
        reserve.ergo_box().ergo_tree.clone(),
        current_height,
    );
    if let Some(tokens) = reserve.ergo_box().tokens.as_ref() {
        for token in tokens {
            builder.add_token(token.clone());
        }
    }
    builder.set_register_value(NonMandatoryRegisterId::R4, reserve.owner.clone().into());
    if let Some((init_height, refund_amount)) = refund_registers {
        builder.set_register_value(NonMandatoryRegisterId::R5, init_height.into());
        builder.set_register_value(NonMandatoryRegisterId::R6, refund_amount.into());
    }
    Ok(builder.build()?)
}

fn build_reserve_refund_tx(
    reserve: &ReserveBoxSpec,
    mut wallet_boxes: Vec<ErgoBox>,
    action: i8,
    output_value: u64,
    refund_registers: Option<(i32, i64)>,
    context: &TxContext,
) -> Result<ReserveResponse<UnsignedTransaction>, TransactionError> {
    let reserve_box_id = reserve.box_id();
    wallet_boxes.retain(|b| b.box_id() != reserve_box_id);
    wallet_boxes.push(reserve.ergo_box().clone());

    let target_balance = output_value
        .checked_add(context.fee)
        .ok_or(ergo_lib::ergotree_ir::chain::ergo_box::box_value::BoxValueError::Overflow)?;
    let target_tokens: Vec<Token> = reserve
        .ergo_box()
        .tokens
        .as_ref()
        .map(|tokens| tokens.as_vec().clone())
        .unwrap_or_else(|| {
            vec![Token {
                token_id: reserve.identifier,
                amount: TokenAmount::try_from(1).unwrap(),
            }]
        });

    let box_selection = SimpleBoxSelector::new().select(
        wallet_boxes,
        target_balance.try_into()?,
        &target_tokens,
    )?;
    let reserve_candidate = build_reserve_action_candidate(
        reserve,
        output_value,
        refund_registers,
        context.current_height,
    )?;

    let mut tx_builder = TxBuilder::new(
        box_selection,
        vec![reserve_candidate],
        context.current_height,
        context.fee.try_into()?,
        NetworkAddress::try_from(context.change_address.clone())?.address(),
    );
    let mut context_extension = ContextExtension::empty();
    // Reserve contract decodes `v = getVar[Byte](0)` as `action = v / 10`, `index = v % 10`.
    // Output #0 is the updated reserve box (`index = 0`).
    context_extension.values.insert(0u8, (action * 10).into());
    tx_builder.set_context_extension(reserve_box_id, context_extension);

    let transaction = tx_builder.build()?;
    let reserve_box = ReserveBoxSpec::try_from(transaction.outputs().first().unwrap())?;
    Ok(ReserveResponse {
        reserve_box,
        transaction,
    })
}

/// Builds an unsigned transaction initiating a timelocked refund on `reserve` (`action == 2`).
///
/// No funds leave the reserve at initiation; `R5` is set to `context.current_height` (`Int`)
/// and `R6` is set to the requested refund cap (`Long`). If `requested_amount` is `None`,
/// defaults to the maximum refundable balance (`reserve_value - SAFE_USER_MIN`).
pub fn initiate_refund_transaction(
    reserve: &ReserveBoxSpec,
    wallet_boxes: Vec<ErgoBox>,
    requested_amount: Option<u64>,
    context: &TxContext,
) -> Result<ReserveResponse<UnsignedTransaction>, TransactionError> {
    if reserve.has_active_refund() {
        return Err(TransactionError::RefundAlreadyActive(reserve.identifier));
    }
    let reserve_value = *reserve.ergo_box().value.as_u64();
    let max_refundable = reserve.max_refundable_nanoergs();
    let refund_amount = requested_amount.unwrap_or(max_refundable);

    let Ok(refund_amount_i64) = i64::try_from(refund_amount) else {
        return Err(TransactionError::InvalidRefundAmount {
            requested: refund_amount,
            max_refundable,
            reserve_value,
        });
    };
    if refund_amount == 0 || refund_amount > max_refundable {
        return Err(TransactionError::InvalidRefundAmount {
            requested: refund_amount,
            max_refundable,
            reserve_value,
        });
    }
    let init_height = i32::try_from(context.current_height)
        .map_err(|_| TransactionError::Parsing("block height exceeds i32::MAX".to_string()))?;

    build_reserve_refund_tx(
        reserve,
        wallet_boxes,
        ACTION_INIT_REFUND,
        reserve_value,
        Some((init_height, refund_amount_i64)),
        context,
    )
}

/// Builds an unsigned transaction cancelling an active pending refund on `reserve` (`action == 3`).
///
/// Clears both `R5` and `R6` on the reserve output box while preserving its balance, script,
/// tokens, and owner public key (`R4`).
pub fn cancel_refund_transaction(
    reserve: &ReserveBoxSpec,
    wallet_boxes: Vec<ErgoBox>,
    context: &TxContext,
) -> Result<ReserveResponse<UnsignedTransaction>, TransactionError> {
    if !reserve.has_active_refund() {
        return Err(TransactionError::NoActiveRefund(reserve.identifier));
    }
    let reserve_value = *reserve.ergo_box().value.as_u64();
    build_reserve_refund_tx(
        reserve,
        wallet_boxes,
        ACTION_CANCEL_REFUND,
        reserve_value,
        None,
        context,
    )
}

/// Builds an unsigned transaction completing an unlocked refund on `reserve` (`action == 4`).
///
/// Withdraws up to the announced `R6` limit, capped at `reserve.max_refundable_nanoergs()` in case
/// note redemptions partially drained the reserve during the timelock window. Clears `R5` and `R6`
/// on the surviving reserve output box and sends the refunded ERG to `context.change_address`.
pub fn complete_refund_transaction(
    reserve: &ReserveBoxSpec,
    wallet_boxes: Vec<ErgoBox>,
    context: &TxContext,
) -> Result<CompletedRefundUnsigned, TransactionError> {
    let Some(init_height) = reserve.refund_height else {
        return Err(TransactionError::NoActiveRefund(reserve.identifier));
    };
    let reserve_value = *reserve.ergo_box().value.as_u64();
    let max_refundable = reserve.max_refundable_nanoergs();

    let Some(refund_limit) = reserve.refund_amount else {
        return Err(TransactionError::InvalidRefundAmount {
            requested: 0,
            max_refundable,
            reserve_value,
        });
    };
    let unlock_height = calculate_refund_unlock_height(init_height);
    if context.current_height < unlock_height {
        return Err(TransactionError::RefundTimelockActive {
            reserve_id: reserve.identifier,
            unlock_height,
            current_height: context.current_height,
        });
    }
    if refund_limit <= 0 {
        return Err(TransactionError::InvalidRefundAmount {
            requested: 0,
            max_refundable,
            reserve_value,
        });
    }
    let requested = refund_limit as u64;
    let refunded_amount = requested.min(max_refundable);
    if refunded_amount == 0 {
        return Err(TransactionError::InvalidRefundAmount {
            requested,
            max_refundable,
            reserve_value,
        });
    }

    let ReserveResponse {
        reserve_box,
        transaction,
    } = build_reserve_refund_tx(
        reserve,
        wallet_boxes,
        ACTION_COMPLETE_REFUND,
        reserve_value - refunded_amount,
        None,
        context,
    )?;

    Ok(CompletedRefundUnsigned {
        reserve_box,
        transaction,
        refunded_amount,
    })
}

#[cfg(test)]
mod test {
    use ergo_lib::{
        chain::{
            ergo_box::box_builder::ErgoBoxCandidateBuilder,
            ergo_state_context::ErgoStateContext,
            transaction::{unsigned::UnsignedTransaction, Transaction},
        },
        ergotree_interpreter::sigma_protocol::private_input::DlogProverInput,
        ergotree_ir::{
            chain::{
                address::{Address, AddressEncoder, NetworkAddress, NetworkPrefix},
                ergo_box::{box_value::BoxValue, ErgoBox, NonMandatoryRegisterId},
                token::Token,
            },
            mir::constant::{Literal, TryExtractFrom, TryExtractInto},
        },
        wallet::{signing::TransactionContext, Wallet},
    };

    use crate::{
        boxes::{Error as BoxSpecError, ReserveBoxSpec},
        test_util::{
            create_box, create_reserve, create_reserve_with_pending_refund, create_wallet_box,
            force_any_val, RESERVE_ADDRESS,
        },
        transactions::{TransactionError, TxContext},
    };

    use super::{
        calculate_refund_unlock_height, cancel_refund_transaction, complete_refund_transaction,
        initiate_refund_transaction, top_up_reserve_transaction, REFUND_TIMELOCK_BLOCKS,
    };

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

    const INITIAL_RESERVE_NANOERGS: u64 = 10_000_000_000;

    fn state_context_at_height(height: u32) -> ErgoStateContext {
        let mut ctx = force_any_val::<ErgoStateContext>();
        ctx.pre_header.height = height;
        ctx
    }

    fn make_tx_context(secret: &DlogProverInput, height: u32) -> TxContext {
        TxContext {
            current_height: height,
            change_address: NetworkAddress::new(
                NetworkPrefix::Mainnet,
                &Address::P2Pk(secret.public_image()),
            )
            .to_base58(),
            fee: *BoxValue::SAFE_USER_MIN.as_u64(),
        }
    }

    fn sign_against_reserve_contract(
        unsigned_tx: UnsignedTransaction,
        reserve: &ReserveBoxSpec,
        mut wallet_boxes: Vec<ErgoBox>,
        secret: DlogProverInput,
        state_ctx: &ErgoStateContext,
    ) -> Transaction {
        wallet_boxes.push(reserve.ergo_box().clone());
        let tx_ctx = TransactionContext::new(unsigned_tx, wallet_boxes, vec![]).unwrap();
        Wallet::from_secrets(vec![secret.into()])
            .sign_transaction(tx_ctx, state_ctx, None)
            .unwrap()
    }

    fn read_reg<T: TryExtractFrom<Literal>>(
        ergo_box: &ErgoBox,
        reg_id: NonMandatoryRegisterId,
    ) -> Option<T> {
        ergo_box
            .get_register(reg_id.into())
            .unwrap()
            .map(|c| c.v.try_extract_into::<T>().unwrap())
    }

    #[test]
    fn test_initiate_refund_explicit_and_default_max_amount() {
        let height = 1_318_639;
        let requested = 4_000_000_000;
        let secret = DlogProverInput::random();
        let pubkey = *secret.public_image().h.clone();
        let reserve = create_reserve(pubkey.clone(), INITIAL_RESERVE_NANOERGS);
        let ctx = make_tx_context(&secret, height);
        let wallet_boxes = vec![create_wallet_box(pubkey.clone(), ctx.fee * 2)];

        let res =
            initiate_refund_transaction(&reserve, wallet_boxes.clone(), Some(requested), &ctx)
                .unwrap();
        assert_eq!(res.reserve_box.refund_height, Some(height as i32));
        assert_eq!(res.reserve_box.refund_amount, Some(requested as i64));
        assert_eq!(
            res.reserve_box.refund_unlock_height(),
            Some(height + REFUND_TIMELOCK_BLOCKS)
        );

        let signed = sign_against_reserve_contract(
            res.transaction,
            &reserve,
            wallet_boxes.clone(),
            secret.clone(),
            &state_context_at_height(height),
        );
        let out = signed.outputs.first();
        assert_eq!(*out.value.as_u64(), INITIAL_RESERVE_NANOERGS);
        assert_eq!(
            read_reg::<i32>(out, NonMandatoryRegisterId::R5),
            Some(height as i32)
        );
        assert_eq!(
            read_reg::<i64>(out, NonMandatoryRegisterId::R6),
            Some(requested as i64)
        );

        // Omitting `requested_amount` defaults to full refundable collateral (`value - SAFE_USER_MIN`)
        let default_res =
            initiate_refund_transaction(&reserve, wallet_boxes.clone(), None, &ctx).unwrap();
        let expected_max = INITIAL_RESERVE_NANOERGS - *BoxValue::SAFE_USER_MIN.as_u64();
        assert_eq!(
            default_res.reserve_box.refund_amount,
            Some(expected_max as i64)
        );
        sign_against_reserve_contract(
            default_res.transaction,
            &reserve,
            wallet_boxes,
            secret,
            &state_context_at_height(height),
        );
    }

    #[test]
    fn test_initiate_refund_validation_errors() {
        let secret = DlogProverInput::random();
        let pubkey = *secret.public_image().h.clone();
        let reserve = create_reserve(pubkey.clone(), INITIAL_RESERVE_NANOERGS);
        let ctx = make_tx_context(&secret, 1_000_000);
        let wallet_boxes = vec![create_wallet_box(pubkey.clone(), ctx.fee * 2)];

        for invalid_amount in [0, INITIAL_RESERVE_NANOERGS, INITIAL_RESERVE_NANOERGS + 500] {
            assert!(matches!(
                initiate_refund_transaction(
                    &reserve,
                    wallet_boxes.clone(),
                    Some(invalid_amount),
                    &ctx
                ),
                Err(TransactionError::InvalidRefundAmount { .. })
            ));
        }

        // Duplicate refund initiation on an already-active reserve is rejected
        let active_reserve = create_reserve_with_pending_refund(
            pubkey.clone(),
            INITIAL_RESERVE_NANOERGS,
            Some((999_500, 2_000_000_000)),
        );
        assert!(matches!(
            initiate_refund_transaction(
                &active_reserve,
                wallet_boxes.clone(),
                Some(1_000_000_000),
                &ctx
            ),
            Err(TransactionError::RefundAlreadyActive(_))
        ));

        // Reserve at minimum box value cannot initiate a refund even with default amount
        let min_reserve = create_reserve(pubkey, *BoxValue::SAFE_USER_MIN.as_u64());
        assert!(matches!(
            initiate_refund_transaction(&min_reserve, wallet_boxes, None, &ctx),
            Err(TransactionError::InvalidRefundAmount {
                max_refundable: 0,
                ..
            })
        ));
    }

    #[test]
    fn test_cancel_refund_clears_registers_and_validates_on_contract() {
        let height = 1_000_500;
        let secret = DlogProverInput::random();
        let pubkey = *secret.public_image().h.clone();
        let active_reserve = create_reserve_with_pending_refund(
            pubkey.clone(),
            INITIAL_RESERVE_NANOERGS,
            Some((1_000_000, 5_000_000_000)),
        );
        let ctx = make_tx_context(&secret, height);
        let wallet_boxes = vec![create_wallet_box(pubkey.clone(), ctx.fee * 2)];

        let res = cancel_refund_transaction(&active_reserve, wallet_boxes.clone(), &ctx).unwrap();
        assert!(!res.reserve_box.has_active_refund());
        assert_eq!(res.reserve_box.refund_height, None);
        assert_eq!(res.reserve_box.refund_amount, None);

        let signed = sign_against_reserve_contract(
            res.transaction,
            &active_reserve,
            wallet_boxes,
            secret.clone(),
            &state_context_at_height(height),
        );
        let out = signed.outputs.first();
        assert_eq!(*out.value.as_u64(), INITIAL_RESERVE_NANOERGS);
        assert_eq!(read_reg::<i32>(out, NonMandatoryRegisterId::R5), None);
        assert_eq!(read_reg::<i64>(out, NonMandatoryRegisterId::R6), None);

        // Cancelling when no refund is active fails with NoActiveRefund
        let idle_reserve = create_reserve(pubkey, INITIAL_RESERVE_NANOERGS);
        assert!(matches!(
            cancel_refund_transaction(&idle_reserve, vec![], &ctx),
            Err(TransactionError::NoActiveRefund(_))
        ));
    }

    #[test]
    fn test_complete_refund_after_timelock_and_lock_enforcement() {
        let init_height: i32 = 1_000_000;
        let unlock_height = calculate_refund_unlock_height(init_height);
        assert_eq!(unlock_height, 1_000_000 + REFUND_TIMELOCK_BLOCKS);

        let refund_amount: u64 = 6_000_000_000;
        let secret = DlogProverInput::random();
        let pubkey = *secret.public_image().h.clone();
        let reserve = create_reserve_with_pending_refund(
            pubkey.clone(),
            INITIAL_RESERVE_NANOERGS,
            Some((init_height, refund_amount as i64)),
        );
        let fee = *BoxValue::SAFE_USER_MIN.as_u64();
        let wallet_boxes = vec![create_wallet_box(pubkey, fee * 2)];

        // 1 block before timelock expiry -> rejected off-chain
        let early_height = unlock_height - 1;
        let early_ctx = make_tx_context(&secret, early_height);
        assert!(matches!(
            complete_refund_transaction(&reserve, wallet_boxes.clone(), &early_ctx),
            Err(TransactionError::RefundTimelockActive { .. })
        ));

        // At exact unlock_height -> builds cleanly
        let ready_ctx = make_tx_context(&secret, unlock_height);
        let completed =
            complete_refund_transaction(&reserve, wallet_boxes.clone(), &ready_ctx).unwrap();
        assert_eq!(completed.refunded_amount, refund_amount);
        assert!(!completed.reserve_box.has_active_refund());

        // Verify the on-chain contract itself ALSO rejects signing 1 block early
        let mut early_inputs = wallet_boxes.clone();
        early_inputs.push(reserve.ergo_box().clone());
        let early_sign_ctx =
            TransactionContext::new(completed.transaction.clone(), early_inputs, vec![]).unwrap();
        assert!(Wallet::from_secrets(vec![secret.clone().into()])
            .sign_transaction(early_sign_ctx, &state_context_at_height(early_height), None)
            .is_err());

        // Verify signing succeeds at unlock_height and outputs match expectations
        let signed = sign_against_reserve_contract(
            completed.transaction,
            &reserve,
            wallet_boxes,
            secret,
            &state_context_at_height(unlock_height),
        );
        let reserve_out = signed.outputs.first();
        assert_eq!(
            *reserve_out.value.as_u64(),
            INITIAL_RESERVE_NANOERGS - refund_amount
        );
        assert_eq!(
            reserve_out.tokens.as_ref().unwrap().first().token_id,
            reserve.identifier
        );
        assert_eq!(
            read_reg::<i32>(reserve_out, NonMandatoryRegisterId::R5),
            None
        );
        assert_eq!(
            read_reg::<i64>(reserve_out, NonMandatoryRegisterId::R6),
            None
        );
    }

    #[test]
    fn test_complete_refund_caps_at_remaining_balance_and_handles_drained_reserve() {
        let init_height: i32 = 1_000_000;
        let unlock_height = calculate_refund_unlock_height(init_height);
        let announced_limit: i64 = 7_000_000_000;
        let remaining_after_redemptions: u64 = 2_500_000_000;

        let secret = DlogProverInput::random();
        let pubkey = *secret.public_image().h.clone();
        let partially_drained = create_reserve_with_pending_refund(
            pubkey.clone(),
            remaining_after_redemptions,
            Some((init_height, announced_limit)),
        );
        let ctx = make_tx_context(&secret, unlock_height);
        let wallet_boxes = vec![create_wallet_box(pubkey.clone(), ctx.fee * 2)];

        let completed =
            complete_refund_transaction(&partially_drained, wallet_boxes.clone(), &ctx).unwrap();
        let expected_withdrawn = remaining_after_redemptions - *BoxValue::SAFE_USER_MIN.as_u64();
        assert_eq!(completed.refunded_amount, expected_withdrawn);

        let signed = sign_against_reserve_contract(
            completed.transaction,
            &partially_drained,
            wallet_boxes.clone(),
            secret,
            &state_context_at_height(unlock_height),
        );
        assert_eq!(
            *signed.outputs.first().value.as_u64(),
            *BoxValue::SAFE_USER_MIN.as_u64()
        );

        // Completely drained reserve (`SAFE_USER_MIN`) cannot withdraw 0 nanoERG
        let fully_drained = create_reserve_with_pending_refund(
            pubkey,
            *BoxValue::SAFE_USER_MIN.as_u64(),
            Some((init_height, announced_limit)),
        );
        assert!(matches!(
            complete_refund_transaction(&fully_drained, wallet_boxes, &ctx),
            Err(TransactionError::InvalidRefundAmount {
                max_refundable: 0,
                ..
            })
        ));
    }

    #[test]
    fn test_reserve_box_spec_register_types() {
        let pubkey = *DlogProverInput::random().public_image().h.clone();
        let spec = create_reserve_with_pending_refund(
            pubkey.clone(),
            INITIAL_RESERVE_NANOERGS,
            Some((1_318_639, 3_500_000_000)),
        );
        assert_eq!(spec.refund_height, Some(1_318_639));
        assert_eq!(spec.refund_amount, Some(3_500_000_000));
        assert!(spec.has_active_refund());

        // Malformed box with R5 encoded as SLong instead of SInt must return InvalidType
        let mut bad_builder = ErgoBoxCandidateBuilder::new(
            BoxValue::new(INITIAL_RESERVE_NANOERGS).unwrap(),
            AddressEncoder::new(NetworkPrefix::Mainnet)
                .parse_address_from_str(RESERVE_ADDRESS)
                .unwrap()
                .script()
                .unwrap(),
            0,
        );
        bad_builder.set_register_value(NonMandatoryRegisterId::R4, pubkey.into());
        bad_builder.set_register_value(NonMandatoryRegisterId::R5, 1_318_639_i64.into());
        bad_builder.add_token(Token {
            token_id: spec.identifier,
            amount: 1.try_into().unwrap(),
        });
        let bad_box = create_box(bad_builder.build().unwrap());
        assert!(matches!(
            ReserveBoxSpec::try_from(&bad_box),
            Err(BoxSpecError::InvalidType { field, .. }) if field == "refund_height"
        ));
    }
}
