//! The async [`SqliteRecorder`]: a background writer thread fed over an
//! mpsc channel, plus the read-side [`SpendRecorder`] trait implementation.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rusqlite::{params, Connection};

use super::now_unix;
use super::pricing::cost_for_record;
use super::query::{
    group_by_model, query_calls, read_distinct, summarize, CallFilter, CallRow, ModelSummary,
    SummaryRow,
};
use super::schema::{db_path, open};
use crate::spend::recorder::{Distinct, SpendRecorder};
use crate::spend::CallRecord;

/// One message on the writer channel. Plain enum so the writer thread can
/// match on the variant without juggling generics.
enum Msg {
    Record(CallRecord),
    /// Flush + ack — the test path waits on the condvar so it can assert
    /// `record` → `query` deterministically.
    Flush(Arc<(Mutex<bool>, Condvar)>),
    Shutdown,
}

/// SQLite-backed recorder. Owns one background writer thread and a
/// channel into it. Cheap to clone — internally it's an `Arc`.
#[derive(Clone)]
pub struct SqliteRecorder {
    inner: Arc<SqliteRecorderInner>,
}

struct SqliteRecorderInner {
    path: PathBuf,
    tx: Mutex<Option<Sender<Msg>>>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

impl SqliteRecorder {
    /// Open the recorder at `path`. Spawns the writer thread immediately so
    /// records arriving microseconds later have somewhere to land.
    pub fn open(path: PathBuf) -> rusqlite::Result<Self> {
        // Verify the DB can be created / migrated before we hand the path
        // to the writer thread. Surfaces "permission denied" / "no space
        // left" at construction time rather than swallowing it forever on
        // the writer thread.
        let _ = open(&path)?;
        let (tx, rx) = mpsc::channel::<Msg>();
        let writer_path = path.clone();
        let writer = thread::Builder::new()
            .name("mogen-spend-writer".into())
            .spawn(move || writer_loop(writer_path, rx))
            .map_err(|e| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("spawn spend writer: {e}"),
                )))
            })?;
        Ok(Self {
            inner: Arc::new(SqliteRecorderInner {
                path,
                tx: Mutex::new(Some(tx)),
                writer: Mutex::new(Some(writer)),
            }),
        })
    }

    /// Open the recorder at the default `~/.mogen/spend.db` path.
    pub fn open_default() -> rusqlite::Result<Self> {
        let path = db_path().ok_or_else(|| {
            rusqlite::Error::InvalidPath(PathBuf::from("could not resolve mogen home"))
        })?;
        Self::open(path)
    }

    /// Borrow the on-disk DB path. Used by the Studio panel to surface
    /// "Spending database at …" in About / Settings.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Open a fresh read-side connection. Returned to callers that want to
    /// run analytics queries without going through the trait.
    pub fn connection(&self) -> rusqlite::Result<Connection> {
        open(&self.inner.path)
    }
}

impl Drop for SqliteRecorderInner {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.lock().unwrap().take() {
            let _ = tx.send(Msg::Shutdown);
        }
        if let Some(handle) = self.writer.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

impl SpendRecorder for SqliteRecorder {
    fn record(&self, record: CallRecord) {
        let tx = self.inner.tx.lock().unwrap();
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(Msg::Record(record));
        }
    }

    fn query(&self, filter: &CallFilter) -> Vec<CallRow> {
        match self.connection().and_then(|c| query_calls(&c, filter)) {
            Ok(rows) => rows,
            Err(_) => Vec::new(),
        }
    }

    fn summary(&self, filter: &CallFilter) -> SummaryRow {
        self.connection()
            .and_then(|c| summarize(&c, filter))
            .unwrap_or_default()
    }

    fn by_model(&self, filter: &CallFilter) -> Vec<ModelSummary> {
        match self.connection().and_then(|c| group_by_model(&c, filter)) {
            Ok(rows) => rows,
            Err(_) => Vec::new(),
        }
    }

    fn distinct(&self) -> Distinct {
        match self.connection().and_then(|c| read_distinct(&c)) {
            Ok(d) => d,
            Err(_) => Distinct::default(),
        }
    }

    fn flush(&self) {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        {
            let tx = self.inner.tx.lock().unwrap();
            if let Some(tx) = tx.as_ref() {
                if tx.send(Msg::Flush(pair.clone())).is_err() {
                    return;
                }
            } else {
                return;
            }
        }
        let (lock, cv) = &*pair;
        let mut done = lock.lock().unwrap();
        while !*done {
            let r = cv
                .wait_timeout(done, Duration::from_secs(2))
                .unwrap();
            done = r.0;
            if r.1.timed_out() {
                break;
            }
        }
    }
}

fn writer_loop(path: PathBuf, rx: mpsc::Receiver<Msg>) {
    let mut conn = match open(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Record(record) => {
                let _ = insert_record(&mut conn, record);
            }
            Msg::Flush(pair) => {
                let (lock, cv) = &*pair;
                let mut done = lock.lock().unwrap();
                *done = true;
                cv.notify_all();
            }
            Msg::Shutdown => break,
        }
    }
}

fn insert_record(conn: &mut Connection, mut record: CallRecord) -> rusqlite::Result<()> {
    if record.ts == 0 {
        record.ts = now_unix();
    }
    if record.cost_usd <= 0.0 {
        record.cost_usd = cost_for_record(conn, &record).unwrap_or(0.0);
    }
    conn.execute(
        "INSERT INTO calls (
            ts, provider, model, operation,
            prompt_tokens, response_tokens, cached_tokens, image_count,
            cost_usd, scene_path, session_id, success, notes
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            record.ts,
            record.provider,
            record.model,
            record.operation,
            record.prompt_tokens,
            record.response_tokens,
            record.cached_tokens,
            record.image_count,
            record.cost_usd,
            record.scene_path,
            record.session_id,
            if record.success { 1i32 } else { 0 },
            record.notes,
        ],
    )?;
    Ok(())
}
