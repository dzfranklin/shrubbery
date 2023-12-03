use rocksdb::{DBWithThreadMode, MultiThreaded};
use std::path::PathBuf;
use std::sync::Arc;
use ulid::Ulid;

#[derive(Clone, Debug)]
pub struct DocDb(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    db: DBWithThreadMode<MultiThreaded>,
}

impl DocDb {
    pub fn open(path: impl Into<PathBuf>) -> eyre::Result<Self> {
        let path = path.into();
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        let db = DBWithThreadMode::<MultiThreaded>::open(&opts, path.clone())?;
        Ok(Self(Arc::new(Inner { db })))
    }

    pub fn create(&self) -> eyre::Result<u128> {
        let id = Ulid::new().0;
        self.0.db.put(id.to_be_bytes(), b"")?;
        Ok(id)
    }
}
