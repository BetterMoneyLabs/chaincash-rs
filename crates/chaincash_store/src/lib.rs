pub mod ergo_boxes;
pub mod error;
pub mod notes;
pub mod reserves;
pub mod scans;
pub mod schema;

use diesel::connection::SimpleConnection;
use diesel::r2d2::{ConnectionManager, CustomizeConnection, ManageConnection, Pool};
use diesel::SqliteConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use ergo_boxes::ErgoBoxRepository;
pub use error::Error;
use notes::NoteRepository;
use reserves::ReserveRepository;
use scans::ScanRepository;
use std::borrow::BorrowMut;

#[derive(serde::Deserialize, Debug)]
pub struct Config {
    pub url: String,
}

pub trait Update {
    fn has_updates(&self) -> Result<bool, Error>;
    fn update(&self) -> Result<(), Error>;
}

const MIGRATIONS: EmbeddedMigrations = embed_migrations!();
type ConnectionType = SqliteConnection;
type ConnectionPool = Pool<ConnectionManager<ConnectionType>>;

#[derive(Clone)]
pub struct ChainCashStore {
    pool: ConnectionPool,
}

impl ChainCashStore {
    pub fn open<S: Into<String>>(store_url: S) -> Result<Self, Error> {
        #[derive(Debug)]
        struct CustomizedConnection;
        type ConnectionManagerError =
            <ConnectionManager<ConnectionType> as ManageConnection>::Error;
        impl CustomizeConnection<ConnectionType, ConnectionManagerError> for CustomizedConnection {
            fn on_acquire(&self, conn: &mut ConnectionType) -> Result<(), ConnectionManagerError> {
                conn.batch_execute("PRAGMA foreign_keys=ON; PRAGMA busy_timeout = 1000;")
                    .map_err(ConnectionManagerError::QueryError)?;
                Ok(())
            }
        }
        let manager = ConnectionManager::<ConnectionType>::new(store_url);

        Ok(Self {
            pool: Pool::builder()
                .connection_customizer(Box::new(CustomizedConnection))
                .build(manager)?,
        })
    }

    pub fn open_in_memory() -> Result<Self, Error> {
        Self::open(":memory:")
    }

    pub fn notes(&self) -> NoteRepository {
        NoteRepository::new(self.pool.clone())
    }

    pub fn reserves(&self) -> ReserveRepository {
        ReserveRepository::new(self.pool.clone())
    }

    pub fn ergo_boxes(&self) -> ErgoBoxRepository {
        ErgoBoxRepository::new(self.pool.clone())
    }

    pub fn scans(&self) -> ScanRepository {
        ScanRepository::new(self.pool.clone())
    }
}

impl Update for ChainCashStore {
    fn has_updates(&self) -> Result<bool, Error> {
        self.pool
            .get()?
            .borrow_mut()
            .has_pending_migration(MIGRATIONS)
            .map_err(|_| crate::Error::Update("failed to check pending migrations"))
    }

    fn update(&self) -> Result<(), Error> {
        self.pool
            .get()?
            .borrow_mut()
            .run_pending_migrations(MIGRATIONS)
            .map_err(|_| crate::Error::Update("failed to run pending migrations"))?;
        Ok(())
    }
}
