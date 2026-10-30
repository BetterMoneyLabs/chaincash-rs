use ergo_lib::{
    chain::ergo_box::box_builder::ErgoBoxCandidateBuilder,
    ergo_chain_types::EcPoint,
    ergotree_ir::{
        chain::{
            address::{Address, AddressEncoder},
            ergo_box::{box_value::BoxValue, ErgoBox, ErgoBoxCandidate, NonMandatoryRegisterId},
            token::{Token, TokenAmount, TokenId},
        },
        sigma_protocol::sigma_boolean::ProveDlog,
    },
};
use proptest::arbitrary::Arbitrary;
use proptest::{
    strategy::{Strategy, ValueTree},
    test_runner::TestRunner,
};
use rand::{thread_rng, Rng};

use crate::{
    boxes::{Note, ReserveBoxSpec},
    note_history::NoteHistory,
};

pub fn force_any_val<T: Arbitrary>() -> T {
    let mut runner = TestRunner::default();
    proptest::arbitrary::any::<T>()
        .new_tree(&mut runner)
        .unwrap()
        .current()
}
// Pre-compiled note address since we need node to compile contract otherwise
pub const RESERVE_ADDRESS: &'static str = "2Ra2gepQYdZrJBu1rYoYXaZtkNKfpQYgRNLbafkgtzhmVNRsbQmqmCAUJSJWsGJWCJm4JQgAbA8JNJRcEAEfgjskavH2BXFCNeNvqUwupMUZqPtMRNHBZFe7BG6Zc6wqYgG624R3jym5dkNEeZXzYMJZH6wwCyvYpGVGyMqwE8Lb7qdMJK1VXBPfs4RkFKAxPAeSiCC6ndKrgBPJ3UoNpK79UdLfZix6LTgq5EGmzZfxaXAZSy33P89zvNhxT9VxJUNgPc8ucUEc3yhGJGAZoMqnWdAVkh8CATVB3qRLp3BoFoRV89F97N7LZcH3GjxnbtDToPsnBo7791x2eu4q9DCFesFCsWsdo8kUbruoPW98nqya9wdoNAGSvXwzjhU2R7desqGkWwoQjWiT2mG6zNrDXYkR2dq5VTCFcUZPuANF3p1Ymb8ADGM3cTvnLFSPGD6v7jWKPZhzun6FRHEGPnjmA83LVXP8zrtmtT8sKCZ5UpBx3Dw1U92ghvYEQ722bJwHSC4XaEfkXVeYbuwHAfY9E9cDRivZyQ6Zdp3PWFfeYqmRgDDNEFtu7FhuooR4eKceNkWwMbEg7bqU7WEGap9Djde9fhGKR3TjxYZD8hCKyuxmQSFPcz8wncqmqgaeNkHQtty4TuefPSp5ebuxdrqmjt3T7Goyv7f9pBwsEjkVLhLmutDZdzR5813USsf5qV4v2q1Q7LnCcJ6vTaEacGzbuuYsBcUoVZnBySXJj2UbTJt6qbShh5TAhAi1VjC7kSd4Uiye2hZFM1zbECJDJBPjbGEWXcvnAaQQnS3qDyc1JqcZWXWJxpyZQrnvB1qUVbagbGPbdHKX85nQJsdC9PqRafHxgb2vH57UMrEpMBWT9Mj9YGxGSz7L5cGyJnvdaQcm5942k6GCDFxg5TW8p8mSuhNZi5GUJCBwNiZLBfT844WtTNNTSAe63CtA3tCCnnWYxtXSqQsdxB59jVe";
pub const RECEIPT_ADDRESS: &'static str = "dW86NzEXDeRHC2ESgUACnMfsDSoNNpVvGTMF8RG9dmi7fSA3Te6VrCUJyguvhs91FLQwwibUcEPZJmxK7BuJucq3R9G97121F62bBE1sLxxSZNBkm5Kvy665fDp5Mr3VfnCiDhzWSnCeKfLhSBoQxS8KgawuqLdn7gNLiAB";
pub const NOTE_ADDRESS: &'static str = "5ARVo4rkBiy9tNbYHUx2AzdzvkXEuvqST2JDHkAu6ExdVQwk6s6WUNKiPPLXwbRWPBQUKTAZ5iTRQFMKRtiRL6fuhRXugE4vZUUiFY4SRrPXfJSt27rmUjrMmSaKncDXsEepT9etszGSutUgScw5S7QCqHgsbLZZQ4MLX2A8vtoz6pyb9VCoCy32pkrGe5UfyFCCHsqtrG82T73de949FEBcYhCXk1tMNfHDu1UyRGNWLPbWPTd8YAFJ8WYDzmkgVqPfSa4zJpFnSU68E3DzNC9SyjK7bWFoKAXwbadUBmYHv4mQA2T66pnKmMMJimWvhAMqFnjxjdvbVnY6wt6YRhRba2QJW4gEiBA95u4uyFKX7B5qxS2uaKTkbd4awe43aSBf5bqx6ufVCZjCjJeyJuQHbM7jwQe5BJewmHJTjCnRNDBkusUpeypWdsZb5hXyoZCMFSCNE6D9fSGX4uZjgdPb6QaCimKW3VBBVoBQ1oojJBrWkbU9KN8f5bRNhzUwyxvL4KnsZb7FwWbi3rCFWxsAEJUtK9AF5VsCTWdQsXFu9MmkZarQFSd3EFEPTJv9qK6RfaYVhyq3iCt8QsmuC1DbBeedsuCeugMZoDNxEprfn48qj9HYakZyf82nqjDWq2av5qos6kRSUihmuiGdxbqWnt";
pub const BUYBACK_ADDRESS: &'static str = "43xhWcMKGeNA8eJ1oFuJGzcCyiBH84t5ViuMLx5BddqmAew1JjhEXkiL5bJsdBktBhvFkpyFb5WJ5m24jT1Kz978h8Mb9Z61feYbCZjLTuF2skSCfQWvsrXpcsTGY2pnVpZn4fe32CijGymD2M4UKdc1YMU8fh6TRJJUfHCra9xkX98cajwsrWUE6aFC7Ck7y4rKA9vNSNsBr1sCFt2i5dj7Cci5Ez3F8QZ8xMkZioAGh6MCvupHuPefnkPXtqBtZxwZ6ve5Nk24oudcRagnepk3ipTMRyxA57sfaNwXAij98doHk2mBU7Li51TD3REpjifjUPmA6DX1U3UMCnuDgAZzdSkdgxanPWzRbxbxEoDyxrieN7TQyirHK9dBpp6iJjaE6ave1Y8TZWCizBvWUsXcEuLWGZczDumdstzDs6zvLk4f8MWTVRSkqorwFfvDRSYK26TbsUbD5dLUw4QW8QCYVzER542syvMKp1MNUyM8bo9kRD2Vtb75V3eZV1Camr1h46nMQLLw3bUq2T9P1wRqCHb7kfqKpR1hDLMqmpEeNNig4WJ6BVnBqzfT7MVesYinRco881NDvJnVi22JNx7CXdFKjWm88fG3UcjmBpG8LWedKiybSv3GxZdf9reZrigTZpMyBgwc5YWN4uF9pKWVr25ZHMnqZK8r5FdbrW5x4k8bW1hoLCe5aXLKXzBYyBNt1wZk33qCE4gJ5qu4dyD9eR1F32vY787vBYbSXrsNiF5iN1MhSJysFy4pQVvePMrxePQTEuqYytJCDGFxbaMiU3mai1GwJVTgnQysQWceSE959HgYBxALsTGisWGCjjfYN7QxnjPbbD36ybsei9VhYXu6";

pub fn create_box(box_candidate: ErgoBoxCandidate) -> ErgoBox {
    let mut rng = thread_rng();
    ErgoBox::from_box_candidate(
        &box_candidate,
        force_any_val(),
        rng.gen_range(0..i16::MAX as u16),
    )
    .unwrap()
}

pub fn create_oracle_box(nanoerg_per_kg: i64) -> ErgoBox {
    // script doesn't matter since oracle box is only used for data input
    let mut box_candidate = ErgoBoxCandidateBuilder::new(
        BoxValue::SAFE_USER_MIN,
        AddressEncoder::new(ergo_lib::ergotree_ir::chain::address::NetworkPrefix::Mainnet)
            .parse_address_from_str(RESERVE_ADDRESS)
            .unwrap()
            .script()
            .unwrap(),
        0,
    );
    box_candidate.add_token(Token {
        token_id: TokenId::from_base64("PEXymlFlsDD9ter12B+BCPnY9QezFIfdUfSuCP4Hz0o=").unwrap(),
        amount: TokenAmount::try_from(1).unwrap(),
    });
    box_candidate.set_register_value(NonMandatoryRegisterId::R4, nanoerg_per_kg.into());
    create_box(box_candidate.build().unwrap())
}

pub fn create_buyback_box() -> ErgoBox {
    let mut box_candidate = ErgoBoxCandidateBuilder::new(
        BoxValue::SAFE_USER_MIN,
        AddressEncoder::new(ergo_lib::ergotree_ir::chain::address::NetworkPrefix::Mainnet)
            .parse_address_from_str(BUYBACK_ADDRESS)
            .unwrap()
            .script()
            .unwrap(),
        0,
    );
    box_candidate.add_token(Token {
        token_id: TokenId::from_base64("vyTtSvfrWng5xDqmskBpfYGxlhIMg34alBgywmbTdVw=").unwrap(),
        amount: TokenAmount::try_from(1).unwrap(),
    });
    create_box(box_candidate.build().unwrap())
}

pub fn create_wallet_box(public_key: EcPoint, amount: u64) -> ErgoBox {
    let tree = Address::P2Pk(ProveDlog::new(public_key)).script().unwrap();
    let box_candidate = ErgoBoxCandidateBuilder::new(BoxValue::new(amount).unwrap(), tree, 0)
        .build()
        .unwrap();
    create_box(box_candidate)
}

pub fn create_reserve(public_key: EcPoint, amount: u64) -> ReserveBoxSpec {
    create_reserve_with_pending_refund(public_key, amount, None)
}

pub fn create_reserve_with_pending_refund(
    public_key: EcPoint,
    amount: u64,
    pending_refund: Option<(i32, i64)>,
) -> ReserveBoxSpec {
    let mut box_candidate = ErgoBoxCandidateBuilder::new(
        BoxValue::new(amount).unwrap(),
        AddressEncoder::new(ergo_lib::ergotree_ir::chain::address::NetworkPrefix::Mainnet)
            .parse_address_from_str(RESERVE_ADDRESS)
            .unwrap()
            .script()
            .unwrap(),
        0,
    );
    box_candidate.set_register_value(NonMandatoryRegisterId::R4, public_key.into());
    if let Some((init_height, refund_amount)) = pending_refund {
        box_candidate.set_register_value(NonMandatoryRegisterId::R5, init_height.into());
        box_candidate.set_register_value(NonMandatoryRegisterId::R6, refund_amount.into());
    }
    box_candidate.add_token(Token {
        token_id: serde_json::from_str(
            "\"161A3A5250655368566D597133743677397A24432646294A404D635166546A57\"",
        )
        .unwrap(),
        amount: 1.try_into().unwrap(),
    });
    ReserveBoxSpec::try_from(&create_box(box_candidate.build().unwrap())).unwrap()
}

pub fn create_note(public_key: &EcPoint, amount: u64) -> Note {
    let mut note_box_candidate = ErgoBoxCandidateBuilder::new(
        BoxValue::SAFE_USER_MIN,
        AddressEncoder::new(ergo_lib::ergotree_ir::chain::address::NetworkPrefix::Mainnet)
            .parse_address_from_str(NOTE_ADDRESS)
            .unwrap()
            .script()
            .unwrap(),
        0,
    );
    note_box_candidate.add_token(Token {
        token_id: serde_json::from_str(
            "\"4b2d8b7beb3eaac8234d9e61792d270898a43934d6a27275e4f3a044609c9f2a\"",
        )
        .unwrap(),
        amount: amount.try_into().unwrap(),
    });
    let history = NoteHistory::new();
    note_box_candidate.set_register_value(NonMandatoryRegisterId::R4, history.to_avltree().into());
    note_box_candidate.set_register_value(NonMandatoryRegisterId::R5, (*public_key).clone().into());
    note_box_candidate.set_register_value(NonMandatoryRegisterId::R6, 0i64.into());
    Note::new(create_box(note_box_candidate.build().unwrap()), history).unwrap()
}
