//! Turso connection setup and small sync wrappers over its async API.

use std::path::Path;
use std::time::Duration;

pub use turso::Error;
pub type Result<T> = turso::Result<T>;

/// A database handle plus one connection; the `Database` is kept alive with it.
pub struct Db {
    _db: turso::Database,
    pub conn: turso::Connection,
}

/// Open `path` in multi-process WAL mode with no busy handler, as skuld's
/// `connect_locked` does with rusqlite.
pub fn open(path: &Path) -> Result<Db> {
    pollster::block_on(async {
        let builder = turso::Builder::new_local(path.to_str().expect("utf-8 path")).experimental_multiprocess_wal(true);
        // Default Windows IO doesn't support shared WAL coordination.
        #[cfg(windows)]
        let builder = builder.with_io(turso::IoBackend::IOCP);
        let db = builder.build().await?;
        let conn = db.connect()?;
        conn.busy_timeout(Duration::ZERO)?;
        Ok(Db { _db: db, conn })
    })
}

impl Db {
    pub fn batch(&self, sql: &str) -> Result<()> {
        pollster::block_on(self.conn.execute_batch(sql))
    }

    pub fn exec(&self, sql: &str, params: impl turso::IntoParams) -> Result<u64> {
        pollster::block_on(self.conn.execute(sql, params))
    }

    /// First column of the first row, as an integer.
    pub fn int(&self, sql: &str) -> Result<i64> {
        match self.value(sql)? {
            turso::Value::Integer(n) => Ok(n),
            v => Err(Error::ConversionFailure(format!("{sql}: expected integer, got {v:?}"))),
        }
    }

    /// First column of the first row, as text.
    pub fn text(&self, sql: &str) -> Result<String> {
        match self.value(sql)? {
            turso::Value::Text(t) => Ok(t),
            v => Err(Error::ConversionFailure(format!("{sql}: expected text, got {v:?}"))),
        }
    }

    fn value(&self, sql: &str) -> Result<turso::Value> {
        pollster::block_on(async {
            let mut rows = self.conn.query(sql, ()).await?;
            let row = rows.next().await?.ok_or(Error::QueryReturnedNoRows)?;
            row.get_value(0)
        })
    }
}

pub fn is_busy(e: &Error) -> bool {
    matches!(e, Error::Busy(_) | Error::BusySnapshot(_))
}

/// Run `f` until it fails with something other than busy. Uncapped: the
/// condition (another process releasing its lock) is real. Returns the
/// result and how many busy retries it took.
pub fn retry_busy<T>(mut f: impl FnMut() -> Result<T>) -> (Result<T>, u64) {
    let mut busy = 0;
    let mut backoff = Duration::from_millis(1);
    loop {
        match f() {
            Err(ref e) if is_busy(e) => {
                busy += 1;
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_millis(50));
            }
            other => return (other, busy),
        }
    }
}
