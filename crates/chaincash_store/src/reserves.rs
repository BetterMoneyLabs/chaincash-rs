use crate::ergo_boxes::ErgoBox;
use crate::ergo_boxes::ErgoBoxRepository;
use crate::schema;
use crate::ConnectionPool;
use crate::Error;
use chaincash_offchain::boxes::ReserveBoxSpec;
use diesel::dsl::delete;
use diesel::prelude::*;
use ergo_lib::ergo_chain_types::EcPoint;
use ergo_lib::ergotree_ir::chain;
use ergo_lib::ergotree_ir::chain::ergo_box::BoxId;
use ergo_lib::ergotree_ir::chain::token::TokenId;
use std::{borrow::BorrowMut, collections::HashMap};

#[derive(Queryable, Selectable, Associations)]
#[diesel(belongs_to(ErgoBox, foreign_key = box_id))]
#[diesel(table_name = schema::reserves)]
pub struct Reserve {
    pub id: i32,
    pub box_id: i32,
    pub denomination_id: Option<i32>,
    /// NFT ID that uniquely identifies this reserve.
    pub identifier: String,
    /// Owner of the reserve, GE encoded as hex string.
    pub owner: String,
}

#[derive(Insertable)]
#[diesel(table_name = schema::reserves)]
pub struct NewReserve<'a> {
    pub box_id: i32,
    pub denomination_id: Option<i32>,
    pub identifier: &'a str,
    pub owner: &'a str,
}

pub struct ReserveRepository {
    pool: ConnectionPool,
}

impl ReserveRepository {
    pub(crate) fn new(pool: ConnectionPool) -> Self {
        Self { pool }
    }

    pub fn add_or_update(&self, reserve_box: &ReserveBoxSpec) -> Result<Reserve, Error> {
        let mut conn = self.pool.get()?;
        let old_box = schema::reserves::table
            .filter(schema::reserves::identifier.eq(String::from(reserve_box.identifier)))
            .select(schema::reserves::box_id)
            .first::<i32>(&mut conn)
            .optional()?;
        if let Some(old_box_id) = old_box {
            diesel::delete(schema::ergo_boxes::table)
                .filter(schema::ergo_boxes::id.eq(old_box_id))
                .execute(&mut conn)?;
        }
        let ergo_box = reserve_box.ergo_box();
        let created_box = ErgoBoxRepository::add_with_conn(conn.borrow_mut(), ergo_box)?;
        let new_reserve = NewReserve {
            box_id: created_box.id,
            denomination_id: None, // TODO, allow setting different denominations, should be auto detected by inspecting the ErgoBox
            owner: &reserve_box.owner.to_string(),
            identifier: &String::from(reserve_box.identifier),
        };
        let query = diesel::insert_into(schema::reserves::table)
            .values(&new_reserve)
            .returning(Reserve::as_returning());
        Ok(query.get_result(&mut conn)?)
    }

    pub fn get_reserve_by_identifier(&self, identifier: &TokenId) -> Result<ReserveBoxSpec, Error> {
        let mut conn = self.pool.get()?;
        let ergo_box = schema::reserves::table
            .filter(schema::reserves::identifier.eq(String::from(*identifier)))
            .inner_join(schema::ergo_boxes::table)
            .select(ErgoBox::as_select())
            .first(&mut conn)?;
        Ok(ReserveBoxSpec::try_from(
            &chain::ergo_box::ErgoBox::try_from(ergo_box)
                .expect("Failed to parse ErgoBox from database"),
        )
        .expect("Failed to parse ReserveBoxSpec from database"))
    }

    pub fn reserve_boxes_by_pubkeys(
        &self,
        pubkeys: &[EcPoint],
    ) -> Result<Vec<ReserveBoxSpec>, Error> {
        let mut conn = self.pool.get()?;
        let join = schema::reserves::table
            .inner_join(schema::ergo_boxes::table)
            .filter(schema::reserves::owner.eq_any(pubkeys.iter().cloned().map(String::from)))
            .select((Reserve::as_select(), ErgoBox::as_select()))
            .load::<(Reserve, ErgoBox)>(&mut conn)?;
        // Panic here if parsing ReserveBox from database fails
        Ok(join
            .into_iter()
            .map(|(_, ergo_box)| {
                ReserveBoxSpec::try_from(&chain::ergo_box::ErgoBox::try_from(ergo_box)?)
                    .map_err(Into::into)
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()
            .expect("Failed to parse ReserveBoxSpec from database"))
    }

    pub fn reserve_totals_by_owner(&self) -> Result<HashMap<String, u64>, Error> {
        let mut conn = self.pool.get()?;
        let reserves = schema::reserves::table
            .inner_join(schema::ergo_boxes::table)
            .select((Reserve::as_select(), ErgoBox::as_select()))
            .load::<(Reserve, ErgoBox)>(&mut conn)?;

        let mut totals = HashMap::new();
        for (reserve, ergo_box) in reserves {
            let ergo_box = chain::ergo_box::ErgoBox::try_from(ergo_box)
                .expect("Failed to parse ErgoBox from database");
            let reserve_box = ReserveBoxSpec::try_from(&ergo_box)
                .expect("Failed to parse ReserveBoxSpec from database");
            *totals.entry(reserve.owner).or_default() += *reserve_box.ergo_box().value.as_u64();
        }

        Ok(totals)
    }

    /// Delete boxes that are not in latest scan (spent)
    pub fn delete_not_in(&self, ids: impl Iterator<Item = BoxId>) -> Result<Vec<String>, Error> {
        let mut conn = self.pool.get()?;
        let ids = ids.map(|id| id.to_string());
        let spent_boxes = schema::reserves::table
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
