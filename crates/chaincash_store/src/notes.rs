use std::{borrow::BorrowMut, collections::HashMap};

use chaincash_offchain::note_history::NoteHistory;
use diesel::{
    associations::{Associations, GroupedBy, Identifiable},
    delete,
    deserialize::Queryable,
    prelude::Insertable,
    BelongingToDsl, Connection, ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl,
    Selectable, SelectableHelper,
};
use ergo_lib::{
    ergo_chain_types::EcPoint,
    ergotree_ir::chain::{self, ergo_box::BoxId, token::TokenId},
};
use serde::Serialize;

use crate::{
    ergo_boxes::{ErgoBox, ErgoBoxRepository},
    schema, ConnectionPool, ConnectionType, Error,
};

#[derive(Queryable, Identifiable, Selectable, Associations, PartialEq, Serialize)]
#[diesel(belongs_to(ErgoBox, foreign_key = box_id))]
#[diesel(table_name = schema::notes)]
pub struct Note {
    pub id: i32,
    #[serde(skip)]
    box_id: i32,
    pub denomination_id: Option<i32>,
    pub identifier: String,
    pub value: i64,
    pub owner: String,
}

#[derive(Insertable)]
#[diesel(table_name = schema::notes)]
struct NewNote<'a> {
    identifier: &'a str,
    box_id: i32,
    denomination_id: Option<i32>,
    value: i64,
    owner: &'a str,
}

#[derive(Queryable, Identifiable, Selectable, Associations, PartialEq, Serialize)]
#[diesel(table_name = schema::ownership_entries)]
#[diesel(belongs_to(Note))]
pub struct OwnershipEntry {
    #[serde(skip)]
    id: i32,
    #[serde(skip)]
    note_id: i32,
    amount: i64,
    position: i64,
    reserve_nft_id: String,
    #[serde(skip)]
    signature: Vec<u8>,
}

impl TryInto<chaincash_offchain::note_history::OwnershipEntry> for OwnershipEntry {
    type Error = Box<dyn std::error::Error>;

    fn try_into(self) -> Result<chaincash_offchain::note_history::OwnershipEntry, Self::Error> {
        let signature = chaincash_offchain::note_history::Signature::try_from(&self.signature[..])?;
        // TODO: Add FromStr for TokenId and BoxId to avoid this hack
        let reserve_id = TokenId::from(BoxId::try_from(self.reserve_nft_id)?);

        Ok(chaincash_offchain::note_history::OwnershipEntry {
            reserve_id,
            amount: self.amount as u64,
            signature,
        })
    }
}

#[derive(Insertable)]
#[diesel(table_name = schema::ownership_entries)]
#[diesel(belongs_to(Note, foreign_key = note_id))]
struct NewOwnershipEntry {
    note_id: i32,
    amount: i64,
    position: i64,
    reserve_nft_id: String,
    signature: Vec<u8>,
}

/// Note with ownership entries. Used for listing notes. Unlike [`chaincash_offchain::boxes::Note`] this includes primary key to uniquely identify a Note
#[derive(Serialize)]
pub struct NoteWithHistory {
    #[serde(flatten)]
    pub note: Note,
    pub history: Vec<OwnershipEntry>,
}

pub struct StoredNoteContext {
    pub nanoerg: u64,
    pub owner: String,
    pub issuer: String,
    pub signers: Vec<String>,
}

pub struct NoteRepository {
    pool: ConnectionPool,
}

impl NoteRepository {
    pub(crate) fn new(pool: ConnectionPool) -> Self {
        Self { pool }
    }

    fn add_history(
        &self,
        conn: &mut ConnectionType,
        note: &Note,
        note_history: &chaincash_offchain::note_history::NoteHistory,
    ) -> Result<(), Error> {
        let ownership_entries: Vec<NewOwnershipEntry> = note_history
            .ownership_entries()
            .iter()
            .enumerate()
            .map(|(i, ownership_entry)| NewOwnershipEntry {
                note_id: note.id,
                amount: ownership_entry.amount as i64,
                position: i as i64,
                reserve_nft_id: ownership_entry.reserve_id.into(),
                signature: ownership_entry.signature.serialize(),
            })
            .collect();
        diesel::insert_into(schema::ownership_entries::table)
            .values(ownership_entries)
            .execute(conn)?;
        Ok(())
    }

    /// Attempt to load a Note box from database.
    pub fn get_note_box(&self, note_id: i32) -> Result<chaincash_offchain::boxes::Note, Error> {
        let mut conn = self.pool.get()?;
        let (note, ergo_box) = schema::notes::table
            .inner_join(schema::ergo_boxes::table)
            .filter(schema::notes::id.eq(note_id))
            .select((Note::as_select(), ErgoBox::as_select()))
            .first(conn.borrow_mut())?;
        let ownership_entries = OwnershipEntry::belonging_to(&note)
            .select(OwnershipEntry::as_select())
            .load(conn.borrow_mut())?;

        // Note that we unwrap/expect when deserializing boxes from reserves. This is because any failure here is either a bug in chaincash or some sort of DB corruption
        let ergo_box: chain::ergo_box::ErgoBox = ergo_box
            .try_into()
            .expect("Failed to parse ergo box from DB");
        let mut note_history = NoteHistory::new();
        for ownership_entry in ownership_entries {
            note_history
                .add_commitment(
                    ownership_entry
                        .try_into()
                        .expect("Failed to parse ownership entry from DB"),
                )
                .expect("Duplicate ADReserve Key when reading history from DB");
        }

        Ok(chaincash_offchain::boxes::Note::new(ergo_box, note_history)
            .expect("Failed to parse note from DB"))
    }

    pub fn get_by_box_id(
        &self,
        box_id: &BoxId,
    ) -> Result<Option<(i32, chaincash_offchain::boxes::Note)>, Error> {
        let mut conn = self.pool.get()?;
        let note_id = schema::ergo_boxes::table
            .inner_join(schema::notes::table)
            .filter(schema::ergo_boxes::ergo_id.eq(&box_id.to_string()))
            .select(schema::notes::id)
            .first::<i32>(&mut conn)
            .optional()?;
        note_id
            .map(|id| Ok((id, self.get_note_box(id)?)))
            .transpose()
    }

    pub fn notes_by_pubkeys(&self, pubkeys: &[EcPoint]) -> Result<Vec<NoteWithHistory>, Error> {
        let mut conn = self.pool.get()?;
        let notes = schema::notes::table
            .filter(schema::notes::owner.eq_any(pubkeys.iter().cloned().map(String::from)))
            .select(Note::as_select())
            .load(conn.borrow_mut())?;
        Ok(OwnershipEntry::belonging_to(&notes)
            .order_by(schema::ownership_entries::position.asc())
            .load(conn.borrow_mut())?
            .grouped_by(&notes)
            .into_iter()
            .zip(notes)
            .map(|(history, note)| NoteWithHistory { note, history })
            .collect())
    }

    pub fn note_contexts(&self) -> Result<Vec<StoredNoteContext>, Error> {
        let mut conn = self.pool.get()?;
        let notes = schema::notes::table
            .select(Note::as_select())
            .load(conn.borrow_mut())?;
        let reserve_owners: HashMap<String, String> = schema::reserves::table
            .select((schema::reserves::identifier, schema::reserves::owner))
            .load(conn.borrow_mut())?
            .into_iter()
            .collect();

        Ok(OwnershipEntry::belonging_to(&notes)
            .order_by(schema::ownership_entries::position.asc())
            .select(OwnershipEntry::as_select())
            .load(conn.borrow_mut())?
            .grouped_by(&notes)
            .into_iter()
            .zip(notes)
            .map(|(history, note)| {
                let signers: Vec<String> = history
                    .iter()
                    .filter_map(|entry| reserve_owners.get(&entry.reserve_nft_id).cloned())
                    .collect();
                let issuer = signers
                    .first()
                    .cloned()
                    .unwrap_or_else(|| note.owner.clone());

                StoredNoteContext {
                    nanoerg: note.value.try_into().unwrap_or_default(),
                    owner: note.owner,
                    issuer,
                    signers,
                }
            })
            .collect())
    }

    pub fn add_note(&self, note: &chaincash_offchain::boxes::Note) -> Result<Note, Error> {
        let mut conn = self.pool.get()?;
        conn.transaction(|conn| {
            let ergo_box = note.ergo_box();
            let created_box = ErgoBoxRepository::add_with_conn(conn.borrow_mut(), ergo_box)?;
            let new_note = NewNote {
                identifier: &String::from(note.note_id),
                box_id: created_box.id,
                denomination_id: None,
                value: note.amount.into(),
                owner: &note.owner.to_string(),
            };
            let inserted_note = diesel::insert_into(schema::notes::table)
                .values(&new_note)
                .returning(Note::as_returning())
                .get_result(conn.borrow_mut())?;
            self.add_history(conn.borrow_mut(), &inserted_note, &note.history)?;
            Ok(inserted_note)
        })
    }

    pub fn delete_note(&self, note_id: i32) -> Result<(), Error> {
        let mut conn = self.pool.get()?;
        let box_id = schema::notes::table
            .inner_join(schema::ergo_boxes::table)
            .filter(schema::notes::id.eq(note_id))
            .select(schema::ergo_boxes::id)
            .first::<i32>(conn.borrow_mut())?;

        // Delete box id. This will delete note and its ownership entries as well (cascade delete)
        diesel::delete(schema::ergo_boxes::table)
            .filter(schema::ergo_boxes::id.eq(box_id))
            .execute(conn.borrow_mut())?;
        Ok(())
    }

    /// Delete boxes that are not in latest scan (spent)
    pub fn delete_not_in(&self, ids: impl Iterator<Item = BoxId>) -> Result<Vec<String>, Error> {
        let mut conn = self.pool.get()?;
        let ids = ids.map(|id| id.to_string());
        let spent_boxes = schema::notes::table
            .inner_join(schema::ergo_boxes::table)
            .filter(diesel::dsl::not(schema::ergo_boxes::ergo_id.eq_any(ids)))
            .select(schema::ergo_boxes::id)
            .into_boxed();
        let query = delete(schema::ergo_boxes::table)
            .filter(schema::ergo_boxes::id.eq_any(spent_boxes))
            .returning(schema::ergo_boxes::ergo_id);
        query.load(&mut conn).map_err(Into::into)
    }
}
