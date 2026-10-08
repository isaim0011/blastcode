//! SQLite persistence. The graph itself is never stored: only symbols, refs and
//! imports. Edges are resolved at query time, so incremental updates cannot
//! leave stale edges behind. A bounded event journal records what changed.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use serde_json::Value;

use crate::model::FileData;

const SCHEMA_VERSION: &str = "2";
const MAX_EVENTS: i64 = 2000;

const SCHEMA: &str = "
CREATE TABLE files(
    path  TEXT PRIMARY KEY,
    lang  TEXT NOT NULL,
    hash  TEXT NOT NULL,
    mtime INTEGER NOT NULL,
    size  INTEGER NOT NULL,
    lines INTEGER NOT NULL
);
CREATE TABLE symbols(
    id         INTEGER PRIMARY KEY,
    file       TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    qualname   TEXT NOT NULL,
    kind       TEXT NOT NULL,
    signature  TEXT NOT NULL,
    doc        TEXT,
    start_line INTEGER NOT NULL,
    end_line   INTEGER NOT NULL,
    depth      INTEGER NOT NULL,
    parent_id  INTEGER,
    exported   INTEGER NOT NULL,
    params     TEXT,
    has_self   INTEGER NOT NULL
);
CREATE INDEX symbols_name ON symbols(name);
CREATE INDEX symbols_file ON symbols(file);
CREATE TABLE refs(
    id           INTEGER PRIMARY KEY,
    file         TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    qualifier    TEXT,
    kind         TEXT NOT NULL,
    line         INTEGER NOT NULL,
    arg_count    INTEGER,
    enclosing_id INTEGER,
    kwargs       TEXT
);
CREATE INDEX refs_name ON refs(name);
CREATE INDEX refs_file ON refs(file);
CREATE INDEX refs_enclosing ON refs(enclosing_id);
CREATE TABLE imports(
    id       INTEGER PRIMARY KEY,
    file     TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    local    TEXT NOT NULL,
    module   TEXT NOT NULL,
    original TEXT,
    wildcard INTEGER NOT NULL,
    line     INTEGER NOT NULL
);
CREATE INDEX imports_file ON imports(file);
CREATE INDEX imports_original ON imports(original);
CREATE TABLE events(
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    ts      INTEGER NOT NULL,
    file    TEXT NOT NULL,
    kind    TEXT NOT NULL,
    added   TEXT NOT NULL,
    removed TEXT NOT NULL,
    changed TEXT NOT NULL
);
CREATE INDEX events_file ON events(file);
CREATE TABLE ts_aliases(
    dir     TEXT NOT NULL,
    pattern TEXT NOT NULL,
    target  TEXT NOT NULL
);
";

pub struct Store {
    pub conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(15))?;
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let ver: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))
            .optional()?;
        if ver.as_deref() != Some(SCHEMA_VERSION) {
            self.conn.execute_batch(
                "DROP TABLE IF EXISTS refs; DROP TABLE IF EXISTS imports;
                 DROP TABLE IF EXISTS symbols; DROP TABLE IF EXISTS files;
                 DROP TABLE IF EXISTS events; DROP TABLE IF EXISTS ts_aliases;",
            )?;
            self.conn.execute_batch(SCHEMA)?;
            self.conn.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES('schema_version',?1)",
                [SCHEMA_VERSION],
            )?;
        }
        Ok(())
    }

    /// path -> (mtime, size, hash)
    pub fn file_states(&self) -> Result<HashMap<String, (i64, i64, String)>> {
        let mut st = self.conn.prepare("SELECT path,mtime,size,hash FROM files")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?;
        let mut map = HashMap::new();
        for r in rows {
            let (p, v) = r?;
            map.insert(p, v);
        }
        Ok(map)
    }
}

pub fn write_file(tx: &Transaction<'_>, f: &FileData) -> Result<()> {
    tx.execute("DELETE FROM files WHERE path=?1", [&f.path])?;
    tx.execute(
        "INSERT INTO files(path,lang,hash,mtime,size,lines) VALUES(?1,?2,?3,?4,?5,?6)",
        params![f.path, f.lang, f.hash, f.mtime, f.size, f.lines],
    )?;
    let mut ids: Vec<i64> = Vec::with_capacity(f.symbols.len());
    {
        let mut st = tx.prepare_cached(
            "INSERT INTO symbols(file,name,qualname,kind,signature,doc,start_line,end_line,depth,parent_id,exported,params,has_self)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        )?;
        for s in &f.symbols {
            let parent_id = s.parent.map(|p| ids[p]);
            let params_json = s
                .params
                .as_ref()
                .map(|p| serde_json::to_string(p).unwrap_or_default());
            st.execute(params![
                f.path, s.name, s.qualname, s.kind, s.signature, s.doc, s.start_line, s.end_line,
                s.depth, parent_id, s.exported, params_json, s.has_self
            ])?;
            ids.push(tx.last_insert_rowid());
        }
    }
    {
        let mut st = tx.prepare_cached(
            "INSERT INTO refs(file,name,qualifier,kind,line,arg_count,enclosing_id,kwargs) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        )?;
        for r in &f.refs {
            let enc = r.enclosing.map(|e| ids[e]);
            st.execute(params![f.path, r.name, r.qualifier, r.kind, r.line, r.arg_count, enc, r.kwargs])?;
        }
    }
    {
        let mut st = tx.prepare_cached(
            "INSERT INTO imports(file,local,module,original,wildcard,line) VALUES(?1,?2,?3,?4,?5,?6)",
        )?;
        for i in &f.imports {
            st.execute(params![f.path, i.local, i.module, i.original, i.wildcard, i.line])?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ journal

pub struct OldSym {
    pub qualname: String,
    pub kind: String,
    pub signature: String,
}

/// Previously indexed symbols of a file; `None` if the file was not indexed.
pub fn load_old_symbols(conn: &Connection, path: &str) -> Result<Option<Vec<OldSym>>> {
    let exists: Option<i64> = conn
        .query_row("SELECT 1 FROM files WHERE path=?1", [path], |r| r.get(0))
        .optional()?;
    if exists.is_none() {
        return Ok(None);
    }
    let mut st = conn.prepare_cached("SELECT qualname,kind,signature FROM symbols WHERE file=?1 ORDER BY id")?;
    let rows = st.query_map([path], |r| {
        Ok(OldSym { qualname: r.get(0)?, kind: r.get(1)?, signature: r.get(2)? })
    })?;
    Ok(Some(rows.collect::<rusqlite::Result<Vec<_>>>()?))
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub seq: i64,
    pub ts: i64,
    pub file: String,
    pub kind: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<Value>,
}

pub fn record_event(
    tx: &Transaction<'_>,
    file: &str,
    kind: &str,
    added: &[String],
    removed: &[String],
    changed: &[Value],
) -> Result<()> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    tx.execute(
        "INSERT INTO events(ts,file,kind,added,removed,changed) VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            ts,
            file,
            kind,
            serde_json::to_string(added)?,
            serde_json::to_string(removed)?,
            serde_json::to_string(changed)?
        ],
    )?;
    Ok(())
}

pub fn prune_events(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "DELETE FROM events WHERE seq <= (SELECT COALESCE(MAX(seq),0) FROM events) - ?1",
        [MAX_EVENTS],
    )?;
    Ok(())
}

pub fn max_seq(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))?)
}

fn map_event(r: &Row<'_>) -> rusqlite::Result<EventRow> {
    let added: String = r.get(4)?;
    let removed: String = r.get(5)?;
    let changed: String = r.get(6)?;
    Ok(EventRow {
        seq: r.get(0)?,
        ts: r.get(1)?,
        file: r.get(2)?,
        kind: r.get(3)?,
        added: serde_json::from_str(&added).unwrap_or_default(),
        removed: serde_json::from_str(&removed).unwrap_or_default(),
        changed: serde_json::from_str(&changed).unwrap_or_default(),
    })
}

/// Events with `seq > since`. `newest_first` returns the most recent `limit` events.
pub fn events_since(
    conn: &Connection,
    since: i64,
    file: Option<&str>,
    limit: usize,
    newest_first: bool,
) -> Result<Vec<EventRow>> {
    let order = if newest_first { "DESC" } else { "ASC" };
    let limit = limit as i64;
    let rows: Vec<EventRow> = if let Some(f) = file {
        let mut st = conn.prepare(&format!(
            "SELECT seq,ts,file,kind,added,removed,changed FROM events WHERE seq>?1 AND file=?2 ORDER BY seq {order} LIMIT ?3"
        ))?;
        let v = st.query_map(params![since, f, limit], map_event)?.collect::<rusqlite::Result<_>>()?;
        v
    } else {
        let mut st = conn.prepare(&format!(
            "SELECT seq,ts,file,kind,added,removed,changed FROM events WHERE seq>?1 ORDER BY seq {order} LIMIT ?2"
        ))?;
        let v = st.query_map(params![since, limit], map_event)?.collect::<rusqlite::Result<_>>()?;
        v
    };
    Ok(rows)
}

// ---------------------------------------------------------------- ts aliases

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TsAlias {
    pub dir: String,
    pub pattern: String,
    pub target: String,
}

pub fn load_ts_aliases(conn: &Connection) -> Result<Vec<TsAlias>> {
    let mut st = conn.prepare("SELECT dir,pattern,target FROM ts_aliases")?;
    let rows = st.query_map([], |r| Ok(TsAlias { dir: r.get(0)?, pattern: r.get(1)?, target: r.get(2)? }))?;
    let mut v: Vec<TsAlias> = rows.collect::<rusqlite::Result<_>>()?;
    v.sort();
    Ok(v)
}

pub fn replace_ts_aliases(tx: &Transaction<'_>, aliases: &[TsAlias]) -> Result<()> {
    tx.execute("DELETE FROM ts_aliases", [])?;
    let mut st = tx.prepare_cached("INSERT INTO ts_aliases(dir,pattern,target) VALUES(?1,?2,?3)")?;
    for a in aliases {
        st.execute(params![a.dir, a.pattern, a.target])?;
    }
    Ok(())
}
