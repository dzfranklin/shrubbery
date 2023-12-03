use rocksdb::{MultiThreaded, OptimisticTransactionDB};
use std::path::PathBuf;
use std::sync::Arc;
use ulid::Ulid;

#[derive(Clone, Debug)]
pub struct UserDb(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    db: OptimisticTransactionDB<MultiThreaded>,
}

impl UserDb {
    pub fn open(path: impl Into<PathBuf>) -> eyre::Result<Self> {
        let path = path.into();
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        let db = OptimisticTransactionDB::<MultiThreaded>::open(&opts, path.clone())?;
        Ok(Self(Arc::new(Inner { db })))
    }

    pub fn personal_doc(&self, user: &str) -> eyre::Result<u128> {
        let tx = self.0.db.transaction();
        let key = format!("{}:personal_doc", user);
        let bytes = tx.get_for_update(key.clone(), false)?;
        if let Some(bytes) = bytes {
            let mut buf = [0; 16];
            buf.copy_from_slice(&bytes);
            let id = u128::from_be_bytes(buf);
            Ok(id)
        } else {
            let id = Ulid::new().0;
            tx.put(key, id.to_be_bytes())?;
            tx.commit()?;
            Ok(id)
        }
    }
}
