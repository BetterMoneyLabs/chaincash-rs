use std::{borrow::Cow, sync::Arc, time::Duration};

use chaincash_offchain::{
    boxes::{Note, ReserveBoxSpec},
    note_history::{NoteHistory, NoteHistoryError, OwnershipEntry},
};
use chaincash_store::scans::ScanType;
use ergo_client::node::{
    endpoints::scan::{RegisteredScan, Scan, ScanBox, TrackingRule},
    NodeClient, NodeError,
};
use ergo_lib::{
    chain::transaction::{ergo_transaction::ErgoTransaction, Transaction, TxId},
    ergotree_ir::{
        chain::{
            ergo_box::{ErgoBox, RegisterId},
            token::TokenId,
        },
        ergo_tree::ErgoTree,
        serialization::SigmaSerializable,
    },
};
use thiserror::Error;
use tracing::{info, warn};

use crate::ServerState;

#[derive(Error, Debug)]
pub enum ScannerError {
    #[error("Node error {0}")]
    NodeError(#[from] NodeError),
    #[error("Box error {0}")]
    BoxError(#[from] chaincash_offchain::boxes::Error),
    #[error("Store error {0}")]
    StoreError(#[from] chaincash_store::Error),
    #[error("Note history error {0}")]
    NoteHistoryError(#[from] chaincash_offchain::note_history::NoteHistoryError),
    #[error("Data input not found for TX {0}")]
    InvalidTransaction(TxId),
    #[error("Note {0:?} validation failed at TX id: {1}, reserve contract invalid")]
    InvalidReserveBox(TokenId, TxId),
}

struct ContractScan<'a> {
    scan_type: ScanType,
    scan: Scan<'a>,
}

impl<'a> ContractScan<'a> {
    async fn new(state: &ServerState, scan_type: ScanType) -> Result<Self, ScannerError> {
        let contract = match scan_type {
            ScanType::Reserves => state.compiler.reserve_contract().await?,
            ScanType::Notes => state.compiler.note_contract().await?,
            ScanType::Receipts => state.compiler.receipt_contract().await?,
        };
        let scan = Self::contract_scan(format!("Chaincash {} scan", scan_type.to_str()), contract);
        Ok(Self { scan_type, scan })
    }

    fn contract_scan(
        scan_name: impl Into<std::borrow::Cow<'a, str>>,
        contract: &ErgoTree,
    ) -> Scan<'a> {
        Scan {
            scan_name: scan_name.into(),
            wallet_interaction: "off".into(),
            tracking_rule: TrackingRule::And {
                args: vec![TrackingRule::Contains {
                    register: Some(RegisterId::R1),
                    value: contract.sigma_serialize_bytes().unwrap().into(),
                }],
            },
            remove_offchain: true,
        }
    }
    async fn register(
        &'a self,
        state: &ServerState,
    ) -> Result<chaincash_store::scans::Scan<'a>, ScannerError> {
        let scan_id = state.node.endpoints().scan()?.register(&self.scan).await?;
        let store_scan =
            chaincash_store::scans::Scan::new(scan_id, &*self.scan.scan_name, self.scan_type);
        state.store.scans().add(&store_scan)?;
        Ok(store_scan)
    }
}

// Wait for the next block before re-checking scans
async fn wait_scan_block(state: &ServerState) -> Result<(), ScannerError> {
    let wallet = state.node.endpoints().wallet()?;
    let cur_wallet_height = wallet.status().await?.wallet_height;
    while wallet.status().await?.wallet_height == cur_wallet_height {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}

async fn get_transaction(
    node: &NodeClient,
    tx_id: &TxId,
) -> Result<Option<Transaction>, NodeError> {
    let indexer = node.endpoints().blockchain()?;
    let blocks_endpoint = node.endpoints().blocks()?;
    Ok(blocks_endpoint
        .transactions(&indexer.get_transaction_by_id(tx_id).await?.block_id)
        .await?
        .into_iter()
        .find(|tx| tx.id() == *tx_id))
}

// Load scans by type. If node changes then wrong scans will be detected and re-registered
// Returns (needs_rescan, scan_type)
async fn load_scan<'a>(
    state: &ServerState,
    scan_type: ScanType,
    node_scans: &[RegisteredScan<'a>],
) -> Result<(bool, Vec<i32>), ScannerError> {
    let contract_scan = ContractScan::new(state, scan_type).await?;
    let scans = state.store.scans().scans_by_type(scan_type)?;
    let registered: Vec<_> = node_scans
        .iter()
        .filter(|node_scan| {
            scans.iter().any(|scan| {
                node_scan.scan_id == scan.scan_id as u32
                    && node_scan.scan.scan_name == scan.scan_name
            })
        })
        .collect();

    if registered
        .iter()
        .any(|registered| registered.scan == contract_scan.scan)
    {
        return Ok((
            false,
            registered
                .into_iter()
                .map(|scan| scan.scan_id as i32)
                .collect(),
        ));
    } else {
        warn!("Scan invalidated, re-registering");
    }

    let scan_id = contract_scan.register(state).await?.scan_id;
    Ok((
        true,
        registered
            .into_iter()
            .map(|scan| scan.scan_id as i32)
            .chain(std::iter::once(scan_id))
            .collect(),
    ))
}

async fn get_all_scan_boxes(
    scan_ids: &[i32],
    state: &ServerState,
) -> Result<Vec<ScanBox>, ScannerError> {
    let mut scan_boxes = vec![];
    for scan in scan_ids {
        scan_boxes.extend_from_slice(
            &state
                .node
                .extensions()
                .get_all_unspent_boxes(*scan as u32, false)
                .await?,
        );
    }
    Ok(scan_boxes)
}

async fn reserve_scanner(state: Arc<ServerState>, scan_ids: Vec<i32>) -> Result<(), ScannerError> {
    loop {
        let scan_boxes = get_all_scan_boxes(&scan_ids, &state).await?;
        for scan_box in &scan_boxes {
            match ReserveBoxSpec::try_from(&scan_box.ergo_box) {
                Ok(reserve_box) => {
                    state.store.reserves().add_or_update(&reserve_box)?;
                }
                Err(e) => warn!(
                    "Failed to import box {} from scan, err: {e}",
                    scan_box.ergo_box.box_id()
                ),
            }
        }
        state
            .store
            .reserves()
            .delete_not_in(scan_boxes.iter().map(|b| b.ergo_box.box_id()))?
            .into_iter()
            .for_each(|deleted| info!("Deleting box id: {deleted}"));
        wait_scan_block(&state).await?;
    }
}

async fn note_backward_scan(state: &ServerState, note_box: ErgoBox) -> Result<Note, ScannerError> {
    let indexer = &state.node.endpoints().blockchain()?;
    let note_token_id = note_box.tokens.as_ref().unwrap().first().token_id;
    let mut history = Vec::new();
    let mut cur_box = Cow::Borrowed(&note_box);
    'outer: loop {
        if let Some((id, old_note)) = state.store.notes().get_by_box_id(&cur_box.box_id())? {
            history.extend(old_note.history.ownership_entries().iter().rev().cloned());
            state.store.notes().delete_note(id)?;
            break;
        }
        let tx = get_transaction(&state.node, &cur_box.transaction_id)
            .await?
            .unwrap();
        if TokenId::from(tx.inputs.first().box_id) == note_token_id {
            // Found transaction where token was minted. Verify all tokens were sent to note contract
            let output_count = tx
                .outputs()
                .iter()
                .flat_map(|output| output.tokens.as_ref().into_iter().flatten())
                .filter(|t| t.token_id == note_token_id)
                .count();
            if output_count == 1 {
                break;
            }
        }
        let Some(reserve_input) = tx.data_inputs().map(|di| &di[0]) else {
            return Err(ScannerError::InvalidTransaction(tx.id()));
        };
        let reserve_box = ReserveBoxSpec::try_from(
            &indexer.get_box_by_id(&reserve_input.box_id).await?.ergo_box,
        )?;
        if reserve_box.ergo_box().ergo_tree != *state.compiler.reserve_contract().await? {
            return Err(ScannerError::InvalidReserveBox(note_token_id, tx.id()));
        }
        for input in tx.inputs.iter() {
            let input_box = indexer.get_box_by_id(&input.box_id).await?;
            if let Some(token) = input_box
                .ergo_box
                .tokens
                .as_ref()
                .map(|tokens| tokens.first())
            {
                if token.token_id == note_token_id
                    && input_box.ergo_box.ergo_tree == *state.compiler.note_contract().await?
                {
                    let ownership_entry = OwnershipEntry::from_context_extension(
                        *token.amount.as_u64(),
                        reserve_box.identifier,
                        &input.spending_proof.extension,
                    )?;
                    history.push(ownership_entry);
                    cur_box = Cow::Owned(input_box.ergo_box);
                    continue 'outer;
                }
            }
        }
        return Err(ScannerError::InvalidTransaction(tx.id()));
    }
    let note_history = history.into_iter().rev().try_fold(
        NoteHistory::new(),
        |mut history, entry| -> Result<NoteHistory, NoteHistoryError> {
            history.add_commitment(entry)?;
            Ok(history)
        },
    )?;
    let note = Note::new(note_box, note_history)?;
    info!(
        "Added note box id {}, identifier: {:?}",
        note.ergo_box().box_id(),
        note.note_id
    );
    Ok(note)
}

async fn note_scanner(state: Arc<ServerState>, scan_ids: Vec<i32>) -> Result<(), ScannerError> {
    loop {
        let scan_boxes = get_all_scan_boxes(&scan_ids, &state).await.unwrap();
        for scan_box in &scan_boxes {
            let box_id = scan_box.ergo_box.box_id();
            if state
                .store
                .ergo_boxes()
                .get_by_id(box_id)
                .unwrap()
                .is_some()
            {
                info!("Skipping box {}", scan_box.ergo_box.box_id());
                continue;
            }
            match note_backward_scan(&state, scan_box.ergo_box.clone()).await {
                Ok(note) => {
                    state.store.notes().add_note(&note).unwrap();
                }
                Err(e) => warn!(
                    "Filtered invalid note box id {} from scan, error {e:?}",
                    box_id
                ),
            }
        }
        state
            .store
            .notes()
            .delete_not_in(scan_boxes.iter().map(|b| b.ergo_box.box_id()))?
            .into_iter()
            .for_each(|deleted| info!("Deleting box id: {deleted}"));
        wait_scan_block(&state).await.unwrap();
    }
}

pub async fn start_scanner(state: Arc<ServerState>) -> Result<(), ScannerError> {
    if let Err(NodeError::BadRequest(_)) =
        state.node.endpoints().blockchain()?.indexed_height().await
    {
        panic!("/blockchain/indexedHeight failed. Please enable extra indexing: https://docs.ergoplatform.com/node/conf/conf-node/#extra-index");
    };
    let scans = state.node.endpoints().scan()?.list_all().await?;
    let (mut needs_rescan, reserve_scans) = load_scan(&state, ScanType::Reserves, &scans).await?;
    let (rescan, note_scans) = load_scan(&state, ScanType::Notes, &scans).await?;
    needs_rescan |= rescan;
    if needs_rescan {
        //Rescan from block #1,318_639. This height can be increased later when chaincash is deployed
        let _ = state.ensure_wallet_unlocked().await;
        let _ = state.node.endpoints().wallet()?.rescan(1_318_639).await;
    }
    tokio::spawn(reserve_scanner(state.clone(), reserve_scans));
    tokio::spawn(note_scanner(state.clone(), note_scans));
    Ok(())
}
