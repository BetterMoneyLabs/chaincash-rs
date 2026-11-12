use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, State},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chaincash_offchain::{
    boxes::Note as OffchainNote,
    note_history::{NoteHistory, OwnershipEntry as OffchainOwnershipEntry, Signature},
    oracle::oracle_nft,
};
use chaincash_predicate::{
    context::{ContextProvider, NanoErg, NoteContext, PredicateContext},
    predicates::{Accept, Predicate},
};
use chaincash_services::ServerState;
use chaincash_store::notes::{Denomination, StoredNoteWithHistory};
use ergo_client::node::{
    endpoints::blockchain::{IndexQuery, SortDirection},
    NodeClient,
};
use ergo_lib::{
    ergo_chain_types::{Base16DecodedBytes, EcPoint},
    ergotree_ir::{
        chain::{
            ergo_box::{ErgoBox, NonMandatoryRegisterId},
            token::TokenId,
        },
        mir::{avl_tree_data::AvlTreeData, constant::TryExtractInto},
        types::stype::SType,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::ApiError;

const DENOMINATION_TYPE_ERG: i32 = 0;

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
struct CheckNoteResponse {
    accepted: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SignatureInput {
    Hex(String),
    Bytes(Vec<u8>),
}

impl SignatureInput {
    fn parse(&self) -> Result<Signature, ApiError> {
        let raw_bytes = match self {
            SignatureInput::Hex(hex_str) => {
                Base16DecodedBytes::try_from(hex_str.as_str())
                    .map_err(|e| ApiError::InvalidNote(format!("Invalid signature hex: {e}")))?
                    .0
            }
            SignatureInput::Bytes(bytes) => bytes.clone(),
        };
        Signature::try_from(raw_bytes.as_slice())
            .map_err(|e| ApiError::InvalidNote(format!("Invalid note signature: {e}")))
    }
}

#[derive(Deserialize)]
struct HistoryEntryInput {
    #[serde(alias = "reserve_id", alias = "reserveId", alias = "reserveNftId")]
    reserve_nft_id: TokenId,
    amount: Option<u64>,
    position: Option<usize>,
    signature: Option<SignatureInput>,
}

struct NormalizedHistoryEntry {
    reserve_id: TokenId,
    amount: u64,
    signature: Option<Signature>,
}

#[derive(Deserialize)]
struct BoxNoteInput {
    #[serde(alias = "box")]
    ergo_box: ErgoBox,
    history: Option<Vec<HistoryEntryInput>>,
    denomination_id: Option<i32>,
}

#[derive(Deserialize)]
struct StoreNoteInput {
    id: Option<i32>,
    denomination_id: Option<i32>,
    identifier: TokenId,
    value: u64,
    owner: EcPoint,
    #[serde(default)]
    history: Vec<HistoryEntryInput>,
}

enum CheckNoteInput {
    Box(BoxNoteInput),
    StoreNote(StoreNoteInput),
    Context(NoteContext),
}

impl CheckNoteInput {
    fn from_json_value(value: Value) -> Result<Self, ApiError> {
        let Value::Object(map) = value else {
            return Err(ApiError::InvalidNote(
                "Expected a JSON object representing a ChainCash note".to_string(),
            ));
        };

        if map.contains_key("ergo_box") || map.contains_key("box") {
            let parsed = serde_json::from_value::<BoxNoteInput>(Value::Object(map))
                .map_err(|e| ApiError::InvalidNote(e.to_string()))?;
            Ok(CheckNoteInput::Box(parsed))
        } else if map.contains_key("ergoTree") || map.contains_key("additionalRegisters") {
            let ergo_box = serde_json::from_value::<ErgoBox>(Value::Object(map))
                .map_err(|e| ApiError::InvalidNote(e.to_string()))?;
            Ok(CheckNoteInput::Box(BoxNoteInput {
                ergo_box,
                history: None,
                denomination_id: None,
            }))
        } else if map.contains_key("identifier") {
            let parsed = serde_json::from_value::<StoreNoteInput>(Value::Object(map))
                .map_err(|e| ApiError::InvalidNote(e.to_string()))?;
            Ok(CheckNoteInput::StoreNote(parsed))
        } else if map.contains_key("nanoerg")
            && map.contains_key("owner")
            && map.contains_key("issuer")
            && map.contains_key("signers")
        {
            let parsed = serde_json::from_value::<NoteContext>(Value::Object(map))
                .map_err(|e| ApiError::InvalidNote(e.to_string()))?;
            Ok(CheckNoteInput::Context(parsed))
        } else {
            Err(ApiError::InvalidNote(
                "Unrecognized note JSON format: expected an ErgoBox, { ergo_box, history }, { identifier, value, owner, history }, or { nanoerg, owner, issuer, signers }".to_string(),
            ))
        }
    }
}

enum CandidateAmount {
    NanoErg(NanoErg),
    Denominated {
        value: u64,
        denomination: Option<Denomination>,
    },
}

impl CandidateAmount {
    fn needs_gold_oracle(&self) -> bool {
        match self {
            CandidateAmount::NanoErg(_) => false,
            CandidateAmount::Denominated { denomination, .. } => {
                denomination_needs_gold_oracle(denomination.as_ref())
            }
        }
    }

    fn to_nanoerg(&self, gold_nanoerg_per_mg: Option<u64>) -> Result<NanoErg, ApiError> {
        match self {
            CandidateAmount::NanoErg(nanoerg) => Ok(*nanoerg),
            CandidateAmount::Denominated {
                value,
                denomination,
            } => convert_value_to_nanoerg(*value, denomination.as_ref(), gold_nanoerg_per_mg),
        }
    }
}

enum CandidateDedupKey {
    BoxId(String),
    StoreNote {
        id: Option<i32>,
        identifier: String,
        owner: String,
        value: i64,
        reserve_nft_ids: Vec<String>,
    },
    None,
}

struct ResolvedCandidateNote {
    owner: String,
    issuer: String,
    signers: Vec<String>,
    amount: CandidateAmount,
    dedup_key: CandidateDedupKey,
    is_backed: bool,
}

struct ZeroCollateralProvider;

impl ContextProvider for ZeroCollateralProvider {
    fn agent_issued_notes(&self, agent: &str) -> Vec<NoteContext> {
        vec![NoteContext {
            nanoerg: 1,
            owner: String::new(),
            issuer: agent.to_string(),
            signers: vec![],
        }]
    }

    fn agent_reserves_nanoerg(&self, _agent: &str) -> NanoErg {
        0
    }
}

struct AcceptanceContextProvider {
    issued_notes: HashMap<String, Vec<NoteContext>>,
    reserves: HashMap<String, NanoErg>,
    unbacked_candidate: Option<NoteContext>,
}

impl ContextProvider for AcceptanceContextProvider {
    fn agent_issued_notes(&self, agent: &str) -> Vec<NoteContext> {
        if agent.is_empty() {
            return self.unbacked_candidate.iter().cloned().collect();
        }
        self.issued_notes.get(agent).cloned().unwrap_or_default()
    }

    fn agent_reserves_nanoerg(&self, agent: &str) -> NanoErg {
        if agent.is_empty() {
            return 0;
        }
        self.reserves.get(agent).copied().unwrap_or_default()
    }
}

fn denomination_needs_gold_oracle(denomination: Option<&Denomination>) -> bool {
    match denomination {
        Some(denom) if denom.type_ == DENOMINATION_TYPE_ERG => false,
        Some(denom) => denom.nanoerg_per_unit.is_none(),
        None => true,
    }
}

fn convert_value_to_nanoerg(
    value: u64,
    denomination: Option<&Denomination>,
    gold_nanoerg_per_mg: Option<u64>,
) -> Result<NanoErg, ApiError> {
    let unit_rate = match denomination {
        Some(denom) if denom.type_ == DENOMINATION_TYPE_ERG => {
            let rate = denom.nanoerg_per_unit.unwrap_or(1);
            u64::try_from(rate).map_err(|_| {
                ApiError::InvalidNote(format!(
                    "Invalid negative nanoerg_per_unit ({rate}) on denomination {}",
                    denom.id
                ))
            })?
        }
        Some(denom) => match denom.nanoerg_per_unit {
            Some(rate) => u64::try_from(rate).map_err(|_| {
                ApiError::InvalidNote(format!(
                    "Invalid negative nanoerg_per_unit ({rate}) on denomination {}",
                    denom.id
                ))
            })?,
            None => gold_nanoerg_per_mg.ok_or_else(|| {
                ApiError::OracleError(
                    "Missing gold oracle rate for gold-denominated note".to_string(),
                )
            })?,
        },
        None => gold_nanoerg_per_mg.ok_or_else(|| {
            ApiError::OracleError("Missing gold oracle rate for note conversion".to_string())
        })?,
    };

    if unit_rate == 0 {
        return Err(ApiError::InvalidNote(
            "Denomination conversion rate must be positive".to_string(),
        ));
    }

    Ok(value.saturating_mul(unit_rate))
}

fn normalize_history_entries(
    mut entries: Vec<HistoryEntryInput>,
    default_amount: u64,
) -> Result<Vec<NormalizedHistoryEntry>, ApiError> {
    let any_position = entries.iter().any(|e| e.position.is_some());
    if any_position {
        if entries.iter().any(|e| e.position.is_none()) {
            return Err(ApiError::InvalidNote(
                "All history entries must specify position when position is provided".to_string(),
            ));
        }
        entries.sort_by_key(|e| e.position.unwrap_or(0));
        for (idx, entry) in entries.iter().enumerate() {
            if entry.position != Some(idx) {
                return Err(ApiError::InvalidNote(
                    "History entry positions must be contiguous starting at 0".to_string(),
                ));
            }
        }
    }

    entries
        .into_iter()
        .map(|entry| {
            let amount = entry.amount.unwrap_or(default_amount);
            if amount == 0 {
                return Err(ApiError::InvalidNote(
                    "History entry amount must be greater than zero".to_string(),
                ));
            }
            let signature = entry
                .signature
                .as_ref()
                .map(SignatureInput::parse)
                .transpose()?;
            Ok(NormalizedHistoryEntry {
                reserve_id: entry.reserve_nft_id,
                amount,
                signature,
            })
        })
        .collect()
}

fn validate_avl_register_shape(ergo_box: &ErgoBox) -> Result<(), ApiError> {
    let reg = ergo_box
        .get_register(NonMandatoryRegisterId::R4.into())
        .map_err(|e| ApiError::InvalidNote(e.to_string()))?
        .ok_or_else(|| {
            ApiError::InvalidNote("Box field was unexpectedly empty: avl tree".to_string())
        })?;
    if reg.tpe != SType::SAvlTree {
        return Err(ApiError::InvalidNote(format!(
            "Box field 'avl tree' was set to incorrect type: {}",
            reg.tpe
        )));
    }
    let avl = reg
        .try_extract_into::<AvlTreeData>()
        .map_err(|e| ApiError::InvalidNote(e.to_string()))?;
    if avl.key_length != 40 || avl.value_length_opt.is_some() {
        return Err(ApiError::InvalidNote(
            "Note box R4 AVL tree must have key_length = 40 and variable value length".to_string(),
        ));
    }
    Ok(())
}

fn resolve_signers_from_reserves(
    reserve_nft_ids: &[String],
    reserve_owners: &HashMap<String, String>,
) -> (String, Vec<String>) {
    let signers: Vec<String> = reserve_nft_ids
        .iter()
        .map(|reserve_id| reserve_owners.get(reserve_id).cloned().unwrap_or_default())
        .collect();
    let issuer = signers.first().cloned().unwrap_or_default();
    (issuer, signers)
}

fn resolve_optional_denomination(
    state: &ServerState,
    denomination_id: Option<i32>,
) -> Result<Option<Denomination>, ApiError> {
    match denomination_id {
        Some(id) => state
            .store
            .notes()
            .get_denomination(id)?
            .ok_or_else(|| ApiError::InvalidNote(format!("Unknown denomination_id {id}")))
            .map(Some),
        None => Ok(None),
    }
}

fn resolve_candidate_note(
    state: &ServerState,
    input: CheckNoteInput,
    reserve_owners: &HashMap<String, String>,
) -> Result<ResolvedCandidateNote, ApiError> {
    match input {
        CheckNoteInput::Context(ctx) => {
            if ctx.owner.is_empty() {
                return Err(ApiError::InvalidNote(
                    "Note owner public key must not be empty".to_string(),
                ));
            }
            let is_backed = !ctx.issuer.is_empty();
            Ok(ResolvedCandidateNote {
                owner: ctx.owner,
                issuer: ctx.issuer,
                signers: ctx.signers,
                amount: CandidateAmount::NanoErg(ctx.nanoerg),
                dedup_key: CandidateDedupKey::None,
                is_backed,
            })
        }
        CheckNoteInput::StoreNote(store_note) => {
            if store_note.value == 0 {
                return Err(ApiError::InvalidNote(
                    "Note value must be greater than zero".to_string(),
                ));
            }
            let value_i64 = i64::try_from(store_note.value).map_err(|_| {
                ApiError::InvalidNote("Note value exceeds maximum i64 token amount".to_string())
            })?;
            let denomination = resolve_optional_denomination(state, store_note.denomination_id)?;
            let normalized_history =
                normalize_history_entries(store_note.history, store_note.value)?;
            let reserve_nft_ids: Vec<String> = normalized_history
                .iter()
                .map(|entry| String::from(entry.reserve_id))
                .collect();
            let (issuer, signers) = resolve_signers_from_reserves(&reserve_nft_ids, reserve_owners);
            let owner = store_note.owner.to_string();
            let identifier = String::from(store_note.identifier);
            let is_backed = !reserve_nft_ids.is_empty();

            Ok(ResolvedCandidateNote {
                owner: owner.clone(),
                issuer,
                signers,
                amount: CandidateAmount::Denominated {
                    value: store_note.value,
                    denomination,
                },
                dedup_key: CandidateDedupKey::StoreNote {
                    id: store_note.id,
                    identifier,
                    owner,
                    value: value_i64,
                    reserve_nft_ids,
                },
                is_backed,
            })
        }
        CheckNoteInput::Box(box_input) => {
            validate_avl_register_shape(&box_input.ergo_box)?;
            let denomination = resolve_optional_denomination(state, box_input.denomination_id)?;
            let default_amount = box_input
                .ergo_box
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.first().into())
                .map(|t| *t.amount.as_u64())
                .ok_or_else(|| {
                    ApiError::InvalidNote(
                        "Note box missing identifier token at index 0".to_string(),
                    )
                })?;
            let box_id_str = box_input.ergo_box.box_id().to_string();

            let verified_note = match box_input.history {
                Some(entries) if entries.is_empty() => {
                    OffchainNote::new(box_input.ergo_box, NoteHistory::new())
                        .map_err(|e| ApiError::InvalidNote(e.to_string()))?
                }
                Some(entries) => {
                    let normalized = normalize_history_entries(entries, default_amount)?;
                    let any_signatures = normalized.iter().any(|e| e.signature.is_some());
                    if any_signatures {
                        if normalized.iter().any(|e| e.signature.is_none()) {
                            return Err(ApiError::InvalidNote(
                                "All history entries must include signature when verifying an ErgoBox against signatures".to_string(),
                            ));
                        }
                        let mut history = NoteHistory::new();
                        for entry in normalized {
                            history
                                .add_commitment(OffchainOwnershipEntry {
                                    reserve_id: entry.reserve_id,
                                    amount: entry.amount,
                                    signature: entry.signature.expect("checked Some above"),
                                })
                                .map_err(|e| ApiError::InvalidNote(e.to_string()))?;
                        }
                        OffchainNote::new(box_input.ergo_box, history)
                            .map_err(|e| ApiError::InvalidNote(e.to_string()))?
                    } else if let Some((_, stored_note)) = state
                        .store
                        .notes()
                        .get_by_box_id(&box_input.ergo_box.box_id())?
                    {
                        let verified = OffchainNote::new(box_input.ergo_box, stored_note.history)
                            .map_err(|e| ApiError::InvalidNote(e.to_string()))?;
                        let matches_stored = normalized.iter().map(|e| e.reserve_id).eq(verified
                            .history
                            .ownership_entries()
                            .iter()
                            .map(|e| e.reserve_id));
                        if !matches_stored {
                            return Err(ApiError::InvalidNote(
                                "Provided history reserve IDs do not match indexed note history"
                                    .to_string(),
                            ));
                        }
                        verified
                    } else {
                        return Err(ApiError::InvalidNote(
                            "History entries for an unindexed ErgoBox note with non-zero chain length must include signatures to verify R4 AVL digest".to_string(),
                        ));
                    }
                }
                None => {
                    if let Some((_, stored_note)) = state
                        .store
                        .notes()
                        .get_by_box_id(&box_input.ergo_box.box_id())?
                    {
                        OffchainNote::new(box_input.ergo_box, stored_note.history)
                            .map_err(|e| ApiError::InvalidNote(e.to_string()))?
                    } else {
                        match OffchainNote::new(box_input.ergo_box, NoteHistory::new()) {
                            Ok(note) => note,
                            Err(chaincash_offchain::boxes::Error::InvalidChainLength {
                                expected,
                                ..
                            }) if expected > 0 => {
                                return Err(ApiError::InvalidNote(format!(
                                    "Note box has chain length {expected} in R6, but history was not provided and box is not indexed in store"
                                )));
                            }
                            Err(err) => return Err(ApiError::InvalidNote(err.to_string())),
                        }
                    }
                }
            };

            let reserve_nft_ids: Vec<String> = verified_note
                .history
                .ownership_entries()
                .iter()
                .map(|entry| String::from(entry.reserve_id))
                .collect();
            let (issuer, signers) = resolve_signers_from_reserves(&reserve_nft_ids, reserve_owners);
            let is_backed = !reserve_nft_ids.is_empty();

            Ok(ResolvedCandidateNote {
                owner: verified_note.owner.to_string(),
                issuer,
                signers,
                amount: CandidateAmount::Denominated {
                    value: *verified_note.amount.as_u64(),
                    denomination,
                },
                dedup_key: CandidateDedupKey::BoxId(box_id_str),
                is_backed,
            })
        }
    }
}

async fn fetch_gold_nanoerg_per_mg(node: &NodeClient) -> Result<u64, ApiError> {
    let is_mainnet = node
        .endpoints()
        .root()
        .map_err(|e| ApiError::OracleError(e.to_string()))?
        .info()
        .await
        .map_err(|e| ApiError::OracleError(e.to_string()))?
        .network
        == "mainnet";

    let boxes = node
        .endpoints()
        .blockchain()
        .map_err(|e| ApiError::OracleError(e.to_string()))?
        .get_unspent_boxes_by_token_id(
            &String::from(oracle_nft(is_mainnet)),
            IndexQuery {
                offset: 0,
                limit: 1,
                sort_direction: SortDirection::Descending,
                include_unconfirmed: true,
            },
        )
        .await
        .map_err(|e| ApiError::OracleError(e.to_string()))?;

    let oracle_box = &boxes
        .first()
        .ok_or_else(|| ApiError::OracleError("Gold oracle box not found".to_string()))?
        .ergo_box;

    let reg = oracle_box
        .get_register(NonMandatoryRegisterId::R4.into())
        .map_err(|e| ApiError::OracleError(e.to_string()))?
        .ok_or_else(|| ApiError::OracleError("Gold oracle box missing R4 register".to_string()))?;

    let nanoerg_per_kg = reg
        .try_extract_into::<i64>()
        .map_err(|e| ApiError::OracleError(e.to_string()))?;

    let nanoerg_per_mg = u64::try_from(nanoerg_per_kg / 1_000_000).map_err(|_| {
        ApiError::OracleError(format!("Invalid gold oracle rate in R4: {nanoerg_per_kg}"))
    })?;

    if nanoerg_per_mg == 0 {
        return Err(ApiError::OracleError(
            "Gold oracle rate per mg is zero".to_string(),
        ));
    }

    Ok(nanoerg_per_mg)
}

fn find_matching_stored_note_index(
    stored_notes: &[StoredNoteWithHistory],
    dedup_key: &CandidateDedupKey,
) -> Option<usize> {
    match dedup_key {
        CandidateDedupKey::BoxId(box_id) => stored_notes
            .iter()
            .position(|stored| &stored.ergo_box_id == box_id),
        CandidateDedupKey::StoreNote {
            id,
            identifier,
            owner,
            value,
            reserve_nft_ids,
        } => {
            let matches_content = |stored: &StoredNoteWithHistory| -> bool {
                &stored.identifier == identifier
                    && &stored.owner == owner
                    && stored.value == *value
                    && stored.history.len() == reserve_nft_ids.len()
                    && stored
                        .history
                        .iter()
                        .map(|h| h.reserve_nft_id())
                        .eq(reserve_nft_ids.iter().map(String::as_str))
            };

            if let Some(target_id) = id {
                if let Some(idx) = stored_notes
                    .iter()
                    .position(|stored| stored.id == *target_id && matches_content(stored))
                {
                    return Some(idx);
                }
            }

            stored_notes.iter().position(matches_content)
        }
        CandidateDedupKey::None => None,
    }
}

fn predicates_accept<P: ContextProvider>(
    predicates: &[Predicate],
    note: NoteContext,
    provider: P,
) -> bool {
    let context = PredicateContext { note, provider };
    predicates
        .iter()
        .any(|predicate| predicate.accept(&context))
}

async fn get_acceptance(State(state): State<Arc<ServerState>>) -> Result<Response, ApiError> {
    Ok(Json(&state.predicates).into_response())
}

async fn check_note(
    State(state): State<Arc<ServerState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(value) = payload.map_err(|err| ApiError::InvalidNote(err.body_text()))?;
    let input = CheckNoteInput::from_json_value(value)?;
    let reserve_owners = state.store.reserves().owners_by_identifier()?;
    let candidate = resolve_candidate_note(&state, input, &reserve_owners)?;

    let prelim_note = NoteContext {
        nanoerg: 0,
        owner: candidate.owner.clone(),
        issuer: candidate.issuer.clone(),
        signers: candidate.signers.clone(),
    };

    if !state.predicates.iter().any(Predicate::uses_collateral) {
        let accepted = predicates_accept(&state.predicates, prelim_note, ZeroCollateralProvider);
        return Ok(Json(CheckNoteResponse { accepted }).into_response());
    }

    if predicates_accept(&state.predicates, prelim_note, ZeroCollateralProvider) {
        return Ok(Json(CheckNoteResponse { accepted: true }).into_response());
    }

    let stored_notes = state.store.notes().all_notes_with_history()?;
    let matched_stored_idx = find_matching_stored_note_index(&stored_notes, &candidate.dedup_key);

    let needs_oracle = (candidate.is_backed && candidate.amount.needs_gold_oracle())
        || stored_notes.iter().enumerate().any(|(idx, stored)| {
            Some(idx) != matched_stored_idx
                && !stored.history.is_empty()
                && denomination_needs_gold_oracle(stored.denomination.as_ref())
        });

    let gold_nanoerg_per_mg = if needs_oracle {
        Some(fetch_gold_nanoerg_per_mg(&state.node).await?)
    } else {
        None
    };

    let candidate_nanoerg = if candidate.is_backed {
        candidate.amount.to_nanoerg(gold_nanoerg_per_mg)?
    } else {
        match &candidate.amount {
            CandidateAmount::NanoErg(n) => (*n).max(1),
            CandidateAmount::Denominated { value, .. } => (*value).max(1),
        }
    };

    let candidate_context = NoteContext {
        nanoerg: candidate_nanoerg,
        owner: candidate.owner,
        issuer: candidate.issuer,
        signers: candidate.signers,
    };

    let mut issued_notes: HashMap<String, Vec<NoteContext>> = HashMap::new();
    for (idx, stored) in stored_notes.into_iter().enumerate() {
        if Some(idx) == matched_stored_idx || stored.history.is_empty() {
            continue;
        }
        let stored_value = u64::try_from(stored.value).unwrap_or(0);
        if stored_value == 0 {
            continue;
        }
        let reserve_ids: Vec<String> = stored
            .history
            .iter()
            .map(|entry| entry.reserve_nft_id().to_string())
            .collect();
        let (issuer, signers) = resolve_signers_from_reserves(&reserve_ids, &reserve_owners);
        if issuer.is_empty() {
            continue;
        }
        let nanoerg = convert_value_to_nanoerg(
            stored_value,
            stored.denomination.as_ref(),
            gold_nanoerg_per_mg,
        )?;
        issued_notes
            .entry(issuer.clone())
            .or_default()
            .push(NoteContext {
                nanoerg,
                owner: stored.owner,
                issuer,
                signers,
            });
    }

    let unbacked_candidate = if candidate.is_backed && !candidate_context.issuer.is_empty() {
        issued_notes
            .entry(candidate_context.issuer.clone())
            .or_default()
            .push(candidate_context.clone());
        None
    } else {
        Some(candidate_context.clone())
    };

    let provider = AcceptanceContextProvider {
        issued_notes,
        reserves: state.store.reserves().reserve_totals_by_owner()?,
        unbacked_candidate,
    };

    let accepted = predicates_accept(&state.predicates, candidate_context, provider);
    Ok(Json(CheckNoteResponse { accepted }).into_response())
}

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(get_acceptance))
        .route("/checkNote", post(check_note))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use chaincash_offchain::{
        boxes::ReserveBoxSpec,
        oracle::GOLD_ORACLE_NFT,
        transactions::{notes::spend_note_transaction, TxContext},
    };
    use chaincash_store::{ChainCashStore, Update};
    use ergo_lib::{
        chain::{ergo_box::box_builder::ErgoBoxCandidateBuilder, transaction::TxId},
        ergo_chain_types::Digest32,
        ergotree_interpreter::sigma_protocol::private_input::DlogProverInput,
        ergotree_ir::{
            chain::{
                address::{Address, NetworkAddress, NetworkPrefix},
                ergo_box::box_value::BoxValue,
                token::{Token, TokenAmount},
            },
            sigma_protocol::sigma_boolean::ProveDlog,
        },
    };
    use serde_json::json;
    use tower::ServiceExt;

    fn make_store() -> ChainCashStore {
        let store = ChainCashStore::open_in_memory().unwrap();
        store.update().unwrap();
        store
    }

    fn offline_state(store: ChainCashStore, predicates: Vec<Predicate>) -> Arc<ServerState> {
        let node = NodeClient::from_url_str(
            "http://127.0.0.1:9052",
            "hello".to_string(),
            std::time::Duration::from_millis(250),
        )
        .unwrap();
        Arc::new(ServerState::new(node, store, predicates))
    }

    async fn spawn_mock_oracle_node(nanoerg_per_kg: i64) -> String {
        let owner_pk = *DlogProverInput::random().public_image().h;
        let tree = Address::P2Pk(ProveDlog::new(owner_pk)).script().unwrap();
        let mut candidate = ErgoBoxCandidateBuilder::new(BoxValue::SAFE_USER_MIN, tree, 100);
        candidate.add_token(Token {
            token_id: Digest32::try_from(GOLD_ORACLE_NFT.to_string())
                .unwrap()
                .into(),
            amount: TokenAmount::try_from(1u64).unwrap(),
        });
        candidate.set_register_value(NonMandatoryRegisterId::R4, nanoerg_per_kg.into());
        let oracle_box =
            ErgoBox::from_box_candidate(&candidate.build().unwrap(), TxId::zero(), 0).unwrap();
        let mut indexed_box = serde_json::to_value(&oracle_box).unwrap();
        indexed_box["globalIndex"] = json!(1);
        indexed_box["spentTransactionId"] = Value::Null;

        let app = Router::new()
            .route(
                "/info",
                get(|| async {
                    Json(json!({
                        "network": "mainnet",
                        "difficulty": 1,
                        "fullHeight": 100,
                        "headersHeight": 100
                    }))
                }),
            )
            .route(
                "/blockchain/box/unspent/byTokenId/:token_id",
                get(move || {
                    let entry = indexed_box.clone();
                    async move { Json(vec![entry]) }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });
        format!("http://{addr}")
    }

    fn build_wallet_box(pk: EcPoint, amount: u64, seed: u8) -> ErgoBox {
        let tree = Address::P2Pk(ProveDlog::new(pk)).script().unwrap();
        let candidate = ErgoBoxCandidateBuilder::new(BoxValue::new(amount).unwrap(), tree, 0)
            .build()
            .unwrap();
        ErgoBox::from_box_candidate(&candidate, TxId(Digest32::from([seed; 32])), 0).unwrap()
    }

    fn build_reserve(pk: EcPoint, amount_nanoerg: u64, seed: u8) -> ReserveBoxSpec {
        let tree = Address::P2Pk(ProveDlog::new(pk.clone())).script().unwrap();
        let mut candidate =
            ErgoBoxCandidateBuilder::new(BoxValue::new(amount_nanoerg).unwrap(), tree, 0);
        candidate.set_register_value(NonMandatoryRegisterId::R4, pk.into());
        candidate.add_token(Token {
            token_id: Digest32::from([seed; 32]).into(),
            amount: TokenAmount::try_from(1u64).unwrap(),
        });
        let ergo_box = ErgoBox::from_box_candidate(
            &candidate.build().unwrap(),
            TxId(Digest32::from([seed.wrapping_add(100); 32])),
            0,
        )
        .unwrap();
        ReserveBoxSpec::try_from(&ergo_box).unwrap()
    }

    fn build_fresh_note(owner: &EcPoint, gold_amount_mg: u64, seed: u8) -> OffchainNote {
        let tree = Address::P2Pk(ProveDlog::new(owner.clone()))
            .script()
            .unwrap();
        let mut candidate = ErgoBoxCandidateBuilder::new(BoxValue::SAFE_USER_MIN, tree, 0);
        candidate.add_token(Token {
            token_id: Digest32::from([seed; 32]).into(),
            amount: TokenAmount::try_from(gold_amount_mg).unwrap(),
        });
        let history = NoteHistory::new();
        candidate.set_register_value(NonMandatoryRegisterId::R4, history.to_avltree().into());
        candidate.set_register_value(NonMandatoryRegisterId::R5, owner.clone().into());
        candidate.set_register_value(NonMandatoryRegisterId::R6, 0i64.into());
        let ergo_box = ErgoBox::from_box_candidate(
            &candidate.build().unwrap(),
            TxId(Digest32::from([seed.wrapping_add(200); 32])),
            0,
        )
        .unwrap();
        OffchainNote::new(ergo_box, history).unwrap()
    }

    fn spend_offchain_note(
        note: &OffchainNote,
        reserve: &ReserveBoxSpec,
        signer_sk: &DlogProverInput,
        recipient: EcPoint,
        amount_mg: u64,
        seed: u8,
    ) -> (OffchainNote, Option<OffchainNote>) {
        let signer_pk = *signer_sk.public_image().h;
        let fee = *BoxValue::SAFE_USER_MIN.as_u64();
        let wallet_boxes = vec![build_wallet_box(signer_pk, fee * 4, seed)];
        let ctx = TxContext {
            current_height: 10,
            change_address: NetworkAddress::new(
                NetworkPrefix::Mainnet,
                &Address::P2Pk(signer_sk.public_image()),
            )
            .to_base58(),
            fee,
        };
        let resp = spend_note_transaction(
            note,
            reserve,
            signer_sk.w.clone(),
            recipient,
            amount_mg,
            wallet_boxes,
            &ctx,
        )
        .unwrap();
        (resp.recipient_note, resp.change_note)
    }

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    async fn post_check_note(state: Arc<ServerState>, body: &str) -> (StatusCode, Value) {
        let response = crate::Server::router()
            .with_state(state)
            .oneshot(
                Request::post("/api/v1/acceptance/checkNote")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn check_note_returns_false_when_no_predicates_configured() {
        let owner_pk = *DlogProverInput::random().public_image().h;
        let note = build_fresh_note(&owner_pk, 1000, 1);
        let state = offline_state(make_store(), vec![]);

        let body = serde_json::to_string(note.ergo_box()).unwrap();
        let (status, json_body) = post_check_note(state, &body).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body, json!({ "accepted": false }));
    }

    #[tokio::test]
    async fn check_note_accepts_and_rejects_wallet_note_json_roundtrip() {
        let issuer_sk = DlogProverInput::random();
        let issuer_pk = *issuer_sk.public_image().h;
        let recipient_pk = *DlogProverInput::random().public_image().h;
        let other_pk = *DlogProverInput::random().public_image().h;

        let reserve = build_reserve(issuer_pk.clone(), 100_000_000, 10);
        let fresh = build_fresh_note(&issuer_pk, 500, 11);
        let (spent_note, _) =
            spend_offchain_note(&fresh, &reserve, &issuer_sk, recipient_pk.clone(), 500, 12);

        let store = make_store();
        store.reserves().add_or_update(&reserve).unwrap();
        store.notes().add_note(&spent_note).unwrap();

        let issuer_whitelist: Predicate = serde_json::from_value(json!({
            "type": "whitelist",
            "kind": "issuer",
            "agents": [issuer_pk.to_string()]
        }))
        .unwrap();
        let state = offline_state(store.clone(), vec![issuer_whitelist]);

        // Fetch exact JSON emitted by GET /api/v1/notes/byPubkey/:pubkey and feed it into checkNote
        let list_resp = crate::Server::router()
            .with_state(state.clone())
            .oneshot(
                Request::get(format!("/api/v1/notes/byPubkey/{recipient_pk}"))
                    .body(Body::default())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_resp.status(), StatusCode::OK);
        let listed_bytes = to_bytes(list_resp.into_body(), usize::MAX).await.unwrap();
        let listed_notes: Vec<Value> = serde_json::from_slice(&listed_bytes).unwrap();
        assert_eq!(listed_notes.len(), 1);

        let (status, accepted_body) = post_check_note(state, &listed_notes[0].to_string()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(accepted_body, json!({ "accepted": true }));

        let non_matching_whitelist: Predicate = serde_json::from_value(json!({
            "type": "whitelist",
            "kind": "issuer",
            "agents": [other_pk.to_string()]
        }))
        .unwrap();
        let reject_state = offline_state(store, vec![non_matching_whitelist]);
        let (status, rejected_body) =
            post_check_note(reject_state, &listed_notes[0].to_string()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(rejected_body, json!({ "accepted": false }));
    }

    #[tokio::test]
    async fn check_note_verifies_ergo_box_with_cryptographic_signatures_and_rejects_tampering() {
        let issuer_sk = DlogProverInput::random();
        let issuer_pk = *issuer_sk.public_image().h;
        let recipient_pk = *DlogProverInput::random().public_image().h;

        let reserve = build_reserve(issuer_pk.clone(), 500_000_000, 20);
        let fresh = build_fresh_note(&issuer_pk, 1000, 21);
        let (spent_note, _) =
            spend_offchain_note(&fresh, &reserve, &issuer_sk, recipient_pk.clone(), 1000, 22);

        // Note is NOT stored in DB (simulating an off-chain / P2P note offer), only reserve is known.
        let store = make_store();
        store.reserves().add_or_update(&reserve).unwrap();

        let predicate: Predicate = serde_json::from_value(json!({
            "type": "whitelist",
            "kind": "historical",
            "agents": [issuer_pk.to_string()]
        }))
        .unwrap();
        let state = offline_state(store, vec![predicate]);

        let entry = &spent_note.history.ownership_entries()[0];
        let valid_payload = json!({
            "ergo_box": spent_note.ergo_box(),
            "history": [{
                "reserve_nft_id": String::from(entry.reserve_id),
                "amount": entry.amount,
                "position": 0,
                "signature": to_hex(&entry.signature.serialize()),
            }]
        });

        let (status, body) = post_check_note(state.clone(), &valid_payload.to_string()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "accepted": true }));

        // Tamper with the reserve_nft_id so the rebuilt AVL tree digest does not match R4
        let tampered_payload = json!({
            "ergo_box": spent_note.ergo_box(),
            "history": [{
                "reserve_nft_id": String::from(TokenId::from(Digest32::from([99u8; 32]))),
                "amount": entry.amount,
                "position": 0,
                "signature": entry.signature.serialize(),
            }]
        });

        let (status, err_body) = post_check_note(state, &tampered_payload.to_string()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err_body["error"]["detail"]
            .as_str()
            .unwrap()
            .contains("AVL digest"));
    }

    #[tokio::test]
    async fn check_note_accepts_raw_indexed_ergo_box_without_explicit_history() {
        let issuer_sk = DlogProverInput::random();
        let issuer_pk = *issuer_sk.public_image().h;
        let recipient_pk = *DlogProverInput::random().public_image().h;

        let reserve = build_reserve(issuer_pk.clone(), 200_000_000, 30);
        let fresh = build_fresh_note(&issuer_pk, 250, 31);
        let (spent_note, _) =
            spend_offchain_note(&fresh, &reserve, &issuer_sk, recipient_pk, 250, 32);

        let store = make_store();
        store.reserves().add_or_update(&reserve).unwrap();
        store.notes().add_note(&spent_note).unwrap();

        let predicate: Predicate = serde_json::from_value(json!({
            "type": "whitelist",
            "kind": "issuer",
            "agents": [issuer_pk.to_string()]
        }))
        .unwrap();
        let state = offline_state(store, vec![predicate]);

        let raw_box_json = serde_json::to_string(spent_note.ergo_box()).unwrap();
        let (status, body) = post_check_note(state, &raw_box_json).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "accepted": true }));
    }

    #[tokio::test]
    async fn check_note_collateral_converts_gold_via_oracle_and_does_not_double_count_stored_note()
    {
        // Oracle rate: 1_000_000_000_000 nanoErg / kg = 1_000_000 nanoErg / mg
        let oracle_url = spawn_mock_oracle_node(1_000_000_000_000).await;
        let node = NodeClient::from_url_str(
            &oracle_url,
            "hello".to_string(),
            std::time::Duration::from_secs(2),
        )
        .unwrap();

        let issuer_sk = DlogProverInput::random();
        let issuer_pk = *issuer_sk.public_image().h;
        let recipient_pk = *DlogProverInput::random().public_image().h;

        // 1_000 mg gold * 1_000_000 nanoErg/mg = 1_000_000_000 nanoErg (1 ERG)
        let reserve = build_reserve(issuer_pk.clone(), 1_000_000_000, 40);
        let fresh = build_fresh_note(&issuer_pk, 1_000, 41);
        let (spent_note, _) = spend_offchain_note(
            &fresh,
            &reserve,
            &issuer_sk,
            recipient_pk.clone(),
            1_000,
            42,
        );

        let store = make_store();
        store.reserves().add_or_update(&reserve).unwrap();
        // Store the note in DB first (as note_scanner does for on-chain notes)
        store.notes().add_note(&spent_note).unwrap();

        let collateral_100: Predicate = serde_json::from_value(json!({
            "type": "collateral",
            "percent": 100
        }))
        .unwrap();
        let state = Arc::new(ServerState::new(
            node.clone(),
            store.clone(),
            vec![collateral_100],
        ));

        // Checking by raw ErgoBox must NOT double-count the stored note (100% >= 100% -> true)
        let raw_box_json = serde_json::to_string(spent_note.ergo_box()).unwrap();
        let (status, body) = post_check_note(state.clone(), &raw_box_json).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "accepted": true }));

        // Checking by wallet NoteWithHistory JSON (even with `id` omitted) must also NOT double-count
        let wallet_note_without_id = json!({
            "identifier": String::from(spent_note.note_id),
            "value": 1_000,
            "owner": recipient_pk.to_string(),
            "history": [{
                "amount": 1_000,
                "position": 0,
                "reserve_nft_id": String::from(reserve.identifier)
            }]
        });
        let (status, body) = post_check_note(state, &wallet_note_without_id.to_string()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "accepted": true }));

        // Requiring 101% collateral must reject the 100%-backed note
        let collateral_101: Predicate = serde_json::from_value(json!({
            "type": "collateral",
            "percent": 101
        }))
        .unwrap();
        let strict_state = Arc::new(ServerState::new(node, store, vec![collateral_101]));
        let (status, body) = post_check_note(strict_state, &raw_box_json).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "accepted": false }));
    }

    #[tokio::test]
    async fn check_note_collateral_preserves_split_change_notes_sharing_note_id() {
        // 1_000_000 nanoErg / mg
        let oracle_url = spawn_mock_oracle_node(1_000_000_000_000).await;
        let node = NodeClient::from_url_str(
            &oracle_url,
            "hello".to_string(),
            std::time::Duration::from_secs(2),
        )
        .unwrap();

        let issuer_sk = DlogProverInput::random();
        let issuer_pk = *issuer_sk.public_image().h;
        let recipient_pk = *DlogProverInput::random().public_image().h;

        // Reserve has only 900_000_000 nanoErg (90% of 1000 mg total issued across recipient + change)
        let reserve = build_reserve(issuer_pk.clone(), 900_000_000, 50);
        let fresh = build_fresh_note(&issuer_pk, 1_000, 51);
        let (recipient_note, change_note) =
            spend_offchain_note(&fresh, &reserve, &issuer_sk, recipient_pk, 800, 52);
        let change_note = change_note.expect("partial spend produces change note");

        let store = make_store();
        store.reserves().add_or_update(&reserve).unwrap();
        store.notes().add_note(&recipient_note).unwrap();
        store.notes().add_note(&change_note).unwrap();

        let collateral_100: Predicate = serde_json::from_value(json!({
            "type": "collateral",
            "percent": 100
        }))
        .unwrap();
        let state = Arc::new(ServerState::new(node, store, vec![collateral_100]));

        // Even though recipient_note alone is 800 mg (800M nanoErg <= 900M reserve),
        // total issued notes (800 mg + 200 mg change = 1000 mg = 1000M nanoErg) exceed 900M reserve (90% < 100%).
        let raw_box_json = serde_json::to_string(recipient_note.ergo_box()).unwrap();
        let (status, body) = post_check_note(state, &raw_box_json).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "accepted": false }));
    }

    #[tokio::test]
    async fn check_note_collateral_accepts_via_well_collateralized_intermediate_signer() {
        // 1_000_000 nanoErg / mg
        let oracle_url = spawn_mock_oracle_node(1_000_000_000_000).await;
        let node = NodeClient::from_url_str(
            &oracle_url,
            "hello".to_string(),
            std::time::Duration::from_secs(2),
        )
        .unwrap();

        let issuer_sk = DlogProverInput::random();
        let issuer_pk = *issuer_sk.public_image().h;
        let endorser_sk = DlogProverInput::random();
        let endorser_pk = *endorser_sk.public_image().h;
        let final_owner_pk = *DlogProverInput::random().public_image().h;

        // Issuer only has 10% collateral (100M nanoErg for 1000 mg = 1000M nanoErg note)
        let issuer_reserve = build_reserve(issuer_pk.clone(), 100_000_000, 60);
        // Endorser has 100% collateral (1000M nanoErg reserve against a 1000 mg note endorser issued)
        let endorser_reserve = build_reserve(endorser_pk.clone(), 1_000_000_000, 61);

        let endorser_fresh = build_fresh_note(&endorser_pk, 1_000, 62);
        let (endorser_issued_note, _) = spend_offchain_note(
            &endorser_fresh,
            &endorser_reserve,
            &endorser_sk,
            final_owner_pk.clone(),
            1_000,
            63,
        );

        let fresh = build_fresh_note(&issuer_pk, 1_000, 64);
        let (hop1, _) =
            spend_offchain_note(&fresh, &issuer_reserve, &issuer_sk, endorser_pk, 1_000, 65);
        let (hop2, _) = spend_offchain_note(
            &hop1,
            &endorser_reserve,
            &endorser_sk,
            final_owner_pk,
            1_000,
            66,
        );

        let store = make_store();
        store.reserves().add_or_update(&issuer_reserve).unwrap();
        store.reserves().add_or_update(&endorser_reserve).unwrap();
        store.notes().add_note(&endorser_issued_note).unwrap();
        store.notes().add_note(&hop2).unwrap();

        let collateral_100: Predicate = serde_json::from_value(json!({
            "type": "collateral",
            "percent": 100
        }))
        .unwrap();
        let state = Arc::new(ServerState::new(node, store, vec![collateral_100]));

        let raw_box_json = serde_json::to_string(hop2.ergo_box()).unwrap();
        let (status, body) = post_check_note(state, &raw_box_json).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "accepted": true }));
    }

    #[tokio::test]
    async fn check_note_rejects_unbacked_note_and_unknown_reserve_on_collateral_and_issuer_blacklist(
    ) {
        let owner_pk = *DlogProverInput::random().public_image().h;
        let blacklisted_pk = *DlogProverInput::random().public_image().h;

        // Even if `owner_pk` owns a large reserve, an unspent R6 == 0 blank note has no reserve signature!
        let owner_reserve = build_reserve(owner_pk.clone(), 5_000_000_000, 70);
        let unbacked_note = build_fresh_note(&owner_pk, 100, 71);

        let store = make_store();
        store.reserves().add_or_update(&owner_reserve).unwrap();

        let collateral_predicate: Predicate = serde_json::from_value(json!({
            "type": "collateral",
            "percent": 50
        }))
        .unwrap();
        let state = offline_state(store.clone(), vec![collateral_predicate]);
        let (status, body) = post_check_note(
            state,
            &serde_json::to_string(unbacked_note.ergo_box()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "accepted": false }));

        // Unknown reserve in history must NOT bypass Blacklist(Issuer)
        let issuer_blacklist: Predicate = serde_json::from_value(json!({
            "type": "blacklist",
            "kind": "issuer",
            "agents": [blacklisted_pk.to_string()]
        }))
        .unwrap();
        let bl_state = offline_state(store, vec![issuer_blacklist]);
        let unknown_reserve_note = json!({
            "identifier": String::from(unbacked_note.note_id),
            "value": 100,
            "owner": owner_pk.to_string(),
            "history": [{
                "amount": 100,
                "position": 0,
                "reserve_nft_id": String::from(TokenId::from(Digest32::from([77u8; 32])))
            }]
        });
        let (status, body) = post_check_note(bl_state, &unknown_reserve_note.to_string()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "accepted": false }));
    }

    #[tokio::test]
    async fn check_note_or_whitelist_short_circuits_when_oracle_node_is_offline() {
        let trusted_owner = *DlogProverInput::random().public_image().h;
        let fresh = build_fresh_note(&trusted_owner, 500, 80);

        let or_predicate: Predicate = serde_json::from_value(json!({
            "type": "or",
            "conditions": [
                {
                    "type": "whitelist",
                    "kind": "owner",
                    "agents": [trusted_owner.to_string()]
                },
                {
                    "type": "collateral",
                    "percent": 100
                }
            ]
        }))
        .unwrap();

        let state = offline_state(make_store(), vec![or_predicate]);
        let (status, body) =
            post_check_note(state, &serde_json::to_string(fresh.ergo_box()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "accepted": true }));
    }

    #[tokio::test]
    async fn check_note_supports_direct_note_context_json_and_rejects_invalid_requests() {
        let predicate: Predicate = serde_json::from_value(json!({
            "type": "whitelist",
            "kind": "owner",
            "agents": ["owner1"]
        }))
        .unwrap();
        let state = offline_state(make_store(), vec![predicate]);

        let ctx_ok = json!({
            "nanoerg": 1000,
            "owner": "owner1",
            "issuer": "issuer1",
            "signers": ["issuer1"]
        });
        let (status, body) = post_check_note(state.clone(), &ctx_ok.to_string()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "accepted": true }));

        let (bad_status, _) = post_check_note(state.clone(), "{not valid json").await;
        assert_eq!(bad_status, StatusCode::BAD_REQUEST);

        let (unrecognized_status, _) =
            post_check_note(state, &json!({"foo": "bar"}).to_string()).await;
        assert_eq!(unrecognized_status, StatusCode::BAD_REQUEST);
    }
}
