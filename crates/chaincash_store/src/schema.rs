// @generated automatically by Diesel CLI.

diesel::table! {
    denominations (id) {
        id -> Integer,
        #[sql_name = "type"]
        type_ -> Integer,
        nanoerg_per_unit -> Nullable<Integer>,
    }
}

diesel::table! {
    ergo_boxes (id) {
        id -> Integer,
        ergo_id -> Text,
        bytes -> Binary,
    }
}

diesel::table! {
    notes (id) {
        id -> Integer,
        identifier -> Text,
        box_id -> Integer,
        denomination_id -> Nullable<Integer>,
        value -> BigInt,
        owner -> Text,
    }
}

diesel::table! {
    ownership_entries (id) {
        id -> Integer,
        note_id -> Integer,
        amount -> BigInt,
        position -> BigInt,
        reserve_nft_id -> Text,
        signature -> Binary,
    }
}

diesel::table! {
    refunds (id) {
        id -> Integer,
        reserve_id -> Text,
        reserve_box_id -> Nullable<Text>,
        requested_amount -> BigInt,
        refunded_amount -> Nullable<BigInt>,
        init_height -> Integer,
        settled_height -> Nullable<Integer>,
        status -> Text,
        init_tx_id -> Nullable<Text>,
        refund_tx_id -> Nullable<Text>,
        error_message -> Nullable<Text>,
    }
}

diesel::table! {
    reserves (id) {
        id -> Integer,
        identifier -> Text,
        owner -> Text,
        box_id -> Integer,
        denomination_id -> Nullable<Integer>,
    }
}

diesel::table! {
    scans (scan_id) {
        scan_id -> Integer,
        scan_type -> Text,
        scan_name -> Text,
    }
}

diesel::joinable!(notes -> denominations (denomination_id));
diesel::joinable!(notes -> ergo_boxes (box_id));
diesel::joinable!(ownership_entries -> notes (note_id));
diesel::joinable!(reserves -> denominations (denomination_id));
diesel::joinable!(reserves -> ergo_boxes (box_id));

diesel::allow_tables_to_appear_in_same_query!(
    denominations,
    ergo_boxes,
    notes,
    ownership_entries,
    refunds,
    reserves,
    scans,
);
