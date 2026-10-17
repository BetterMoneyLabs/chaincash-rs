use std::borrow::BorrowMut;

use crate::{schema, ConnectionPool, ConnectionType, Error};
use diesel::prelude::*;
use ergo_lib::ergotree_ir::{
    chain::ergo_box::{BoxId, ErgoBox as NetworkBox},
    serialization::{SigmaParsingError, SigmaSerializable},
};

#[derive(Queryable, Selectable, Clone)]
#[diesel(table_name = schema::ergo_boxes)]
pub struct ErgoBox {
    pub id: i32,
    pub ergo_id: String,
    pub bytes: Vec<u8>,
}

#[derive(Insertable)]
#[diesel(table_name = schema::ergo_boxes)]
pub struct NewErgoBox {
    pub ergo_id: String,
    pub bytes: Vec<u8>,
}

impl TryFrom<&NetworkBox> for NewErgoBox {
    type Error = Error;

    fn try_from(value: &NetworkBox) -> Result<Self, Self::Error> {
        Ok(Self {
            ergo_id: value.box_id().to_string(),
            bytes: value.sigma_serialize_bytes().unwrap(),
        })
    }
}

impl TryFrom<ErgoBox> for NetworkBox {
    type Error = SigmaParsingError;
    fn try_from(value: ErgoBox) -> Result<NetworkBox, Self::Error> {
        let parsed = NetworkBox::sigma_parse_bytes(&value.bytes)?;
        assert_eq!(
            parsed.box_id().to_string(),
            value.ergo_id,
            "Box read from database does not have matching box id"
        ); // sanity check
        Ok(parsed)
    }
}
pub struct ErgoBoxRepository {
    pool: ConnectionPool,
}

impl ErgoBoxRepository {
    pub(crate) fn new(pool: ConnectionPool) -> Self {
        Self { pool }
    }

    pub fn add(&self, b: &NetworkBox) -> Result<ErgoBox, Error> {
        Self::add_with_conn(self.pool.get()?.borrow_mut(), b)
    }

    pub(crate) fn add_with_conn(
        conn: &mut ConnectionType,
        b: &NetworkBox,
    ) -> Result<ErgoBox, Error> {
        let new_box = NewErgoBox::try_from(b)?;
        Ok(diesel::insert_into(schema::ergo_boxes::table)
            .values(new_box)
            .returning(ErgoBox::as_returning())
            .get_result(conn)?)
    }

    pub fn get_by_id(&self, box_id: BoxId) -> Result<Option<ErgoBox>, Error> {
        let mut conn = self.pool.get()?;
        Ok(schema::ergo_boxes::table
            .filter(schema::ergo_boxes::ergo_id.eq(box_id.to_string()))
            .select(ErgoBox::as_select())
            .first(&mut conn)
            .optional()?)
    }

    #[allow(dead_code)]
    pub(crate) fn delete_with_conn(conn: &mut ConnectionType, box_id: BoxId) -> Result<(), Error> {
        diesel::delete(schema::ergo_boxes::table)
            .filter(schema::ergo_boxes::ergo_id.eq(box_id.to_string()))
            .execute(conn)?;
        Ok(())
    }
}
