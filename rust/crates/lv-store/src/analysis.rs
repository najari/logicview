//! Decoder results in the capture file (`analyzer`, `frame`; DEC-1, UI-9).

use std::path::Path;

use rusqlite::{Connection, OpenFlags, params};

use crate::{CaptureFile, Result};

/// An analyzer to add: a decoder stack (`kind = 'sigrok'`) or an HLA.
#[derive(Debug, Clone)]
pub struct NewAnalyzer {
    pub kind: String,
    /// The stack in `sigrok-cli -P` form, or the HLA id.
    pub decoder_id: String,
    pub label: String,
    /// Settings and the annotation rows, as JSON (opaque here).
    pub settings_json: String,
    pub input_id: Option<i64>,
}

/// One annotation / frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub start: u64,
    pub end: u64,
    pub row: i64,
    /// `<decoder>.<annotation class>`.
    pub kind: String,
    pub text: String,
    /// Extra data as JSON, e.g. the shorter texts.
    pub data: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AnalyzerInfo {
    pub id: i64,
    pub kind: String,
    pub decoder_id: String,
    pub label: String,
    pub settings_json: String,
    pub input_id: Option<i64>,
    pub frames: u64,
    /// Longest frame in samples (bounds range queries).
    pub max_span: u64,
}

/// One value of a query result.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    /// Blobs are not returned, only their size.
    Blob(usize),
}

#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    /// More rows were available than `max_rows`.
    pub truncated: bool,
}

fn open_rw(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

/// Store an analyzer and its frames in one transaction. Returns its id.
pub fn add_analyzer(path: &Path, a: &NewAnalyzer, frames: &[Frame]) -> Result<i64> {
    let mut conn = open_rw(path)?;
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO analyzer(capture_id, kind, decoder_id, label, settings_json, input_id)
         VALUES ((SELECT id FROM capture ORDER BY id LIMIT 1), ?1, ?2, ?3, ?4, ?5)",
        params![a.kind, a.decoder_id, a.label, a.settings_json, a.input_id],
    )?;
    let id = tx.last_insert_rowid();
    {
        let mut st = tx.prepare(
            "INSERT INTO frame(analyzer_id, start_sample, end_sample, ann_row, type, text, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for f in frames {
            st.execute(params![
                id,
                f.start as i64,
                f.end as i64,
                f.row,
                f.kind,
                f.text,
                f.data
            ])?;
        }
    }
    tx.commit()?;
    Ok(id)
}

/// Remove an analyzer, its frames and analyzers stacked on it.
pub fn delete_analyzer(path: &Path, id: i64) -> Result<()> {
    let mut conn = open_rw(path)?;
    let tx = conn.transaction()?;
    let mut ids = vec![id];
    let mut i = 0;
    while i < ids.len() {
        let mut st = tx.prepare("SELECT id FROM analyzer WHERE input_id = ?1")?;
        let kids: Vec<i64> = st
            .query_map([ids[i]], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        ids.extend(kids);
        i += 1;
    }
    // Children first (analyzer.input_id is a foreign key).
    for a in ids.into_iter().rev() {
        tx.execute("DELETE FROM frame WHERE analyzer_id = ?1", [a])?;
        tx.execute("DELETE FROM analyzer WHERE id = ?1", [a])?;
    }
    tx.commit()?;
    Ok(())
}

fn frame_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Frame)> {
    Ok((
        r.get(0)?,
        Frame {
            start: r.get::<_, i64>(1)? as u64,
            end: r.get::<_, i64>(2)? as u64,
            row: r.get(3)?,
            kind: r.get(4)?,
            text: r.get(5)?,
            data: r.get(6)?,
        },
    ))
}

impl CaptureFile {
    pub fn analyzers(&self) -> Result<Vec<AnalyzerInfo>> {
        let mut st = self.conn().prepare(
            "SELECT a.id, a.kind, a.decoder_id, a.label, a.settings_json, a.input_id,
                    COUNT(f.id), COALESCE(MAX(f.end_sample - f.start_sample), 0)
             FROM analyzer a LEFT JOIN frame f ON f.analyzer_id = a.id
             WHERE a.capture_id = ?1 GROUP BY a.id ORDER BY a.id",
        )?;
        let v = st
            .query_map([self.info.id], |r| {
                Ok(AnalyzerInfo {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    decoder_id: r.get(2)?,
                    label: r.get(3)?,
                    settings_json: r.get(4)?,
                    input_id: r.get(5)?,
                    frames: r.get::<_, i64>(6)? as u64,
                    max_span: r.get::<_, i64>(7)? as u64,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    /// Frames of `analyzer` that overlap `[a, b)`, at most `limit`, in
    /// start order. `max_span` is [`AnalyzerInfo::max_span`].
    pub fn frames_in(
        &self,
        analyzer: i64,
        a: u64,
        b: u64,
        max_span: u64,
        limit: usize,
    ) -> Result<Vec<Frame>> {
        let mut st = self.conn().prepare_cached(
            "SELECT id, start_sample, end_sample, ann_row, type, text, data FROM frame
             WHERE analyzer_id = ?1 AND start_sample >= ?2 AND start_sample < ?3 AND end_sample >= ?4
             ORDER BY start_sample LIMIT ?5",
        )?;
        let lo = a.saturating_sub(max_span) as i64;
        let v = st
            .query_map(
                params![analyzer, lo, b as i64, a as i64, limit as i64],
                frame_from,
            )?
            .map(|r| r.map(|(_, f)| f))
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    /// Run one read-only SQL statement (AUT-4): at most `max_rows` rows,
    /// interrupted after `timeout`. Statements that would write are refused.
    pub fn query(
        &self,
        sql: &str,
        max_rows: usize,
        timeout: std::time::Duration,
    ) -> std::result::Result<QueryResult, String> {
        let conn = self.conn();
        let mut st = conn.prepare(sql).map_err(|e| format!("SQL error: {e}"))?;
        if !st.readonly() {
            return Err("only read-only statements (SELECT, WITH ... SELECT) are allowed".into());
        }
        let columns: Vec<String> = st.column_names().map(|c| c.to_string()).collect();
        // A watchdog interrupts the statement when it runs too long.
        let handle = conn.get_interrupt_handle();
        let done = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let watchdog = {
            let done = done.clone();
            std::thread::spawn(move || {
                let (lock, cv) = &*done;
                let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                let (guard, res) = cv
                    .wait_timeout_while(guard, timeout, |d| !*d)
                    .unwrap_or_else(|e| e.into_inner());
                if res.timed_out() && !*guard {
                    handle.interrupt();
                }
            })
        };
        let result = (|| {
            let mut rows = Vec::new();
            let mut truncated = false;
            let mut q = st.query([])?;
            while let Some(r) = q.next()? {
                if rows.len() == max_rows {
                    truncated = true;
                    break;
                }
                let mut row = Vec::with_capacity(columns.len());
                for i in 0..columns.len() {
                    use rusqlite::types::ValueRef;
                    row.push(match r.get_ref(i)? {
                        ValueRef::Null => Cell::Null,
                        ValueRef::Integer(v) => Cell::Int(v),
                        ValueRef::Real(v) => Cell::Real(v),
                        ValueRef::Text(t) => Cell::Text(String::from_utf8_lossy(t).into_owned()),
                        ValueRef::Blob(b) => Cell::Blob(b.len()),
                    });
                }
                rows.push(row);
            }
            Ok::<_, rusqlite::Error>((rows, truncated))
        })();
        {
            let (lock, cv) = &*done;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cv.notify_all();
        }
        let _ = watchdog.join();
        match result {
            Ok((rows, truncated)) => Ok(QueryResult {
                columns,
                rows,
                truncated,
            }),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::OperationInterrupted =>
            {
                Err(format!(
                    "query stopped after {:.1} s (time limit)",
                    timeout.as_secs_f64()
                ))
            }
            Err(e) => Err(format!("SQL error: {e}")),
        }
    }

    /// The annotation rows that hold at least one frame.
    pub fn frame_rows(&self, analyzer: i64) -> Result<Vec<i64>> {
        let mut st = self.conn().prepare_cached(
            "SELECT DISTINCT ann_row FROM frame WHERE analyzer_id = ?1 ORDER BY ann_row",
        )?;
        let v = st
            .query_map([analyzer], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    /// Frames matching `search` (text or type contains it; empty = all).
    pub fn frame_count(&self, analyzer: i64, search: &str) -> Result<u64> {
        let pat = format!("%{}%", search);
        let n: i64 = self.conn().prepare_cached(
            "SELECT COUNT(*) FROM frame WHERE analyzer_id = ?1 AND (?2 = '%%' OR text LIKE ?2 OR type LIKE ?2)",
        )?
        .query_row(params![analyzer, pat], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// One page of the data table (UI-9): `(row number, frame)`.
    pub fn frames_page(
        &self,
        analyzer: i64,
        search: &str,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<Frame>> {
        let pat = format!("%{}%", search);
        let mut st = self.conn().prepare_cached(
            "SELECT id, start_sample, end_sample, ann_row, type, text, data FROM frame
             WHERE analyzer_id = ?1 AND (?2 = '%%' OR text LIKE ?2 OR type LIKE ?2)
             ORDER BY start_sample, id LIMIT ?3 OFFSET ?4",
        )?;
        let v = st
            .query_map(
                params![analyzer, pat, limit as i64, offset as i64],
                frame_from,
            )?
            .map(|r| r.map(|(_, f)| f))
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CaptureWriter, NewCapture, Status};

    fn frame(start: u64, end: u64, text: &str) -> Frame {
        Frame {
            start,
            end,
            row: 0,
            kind: "uart.rx-data".into(),
            text: text.into(),
            data: None,
        }
    }

    #[test]
    fn dec1_analyzer_frames_roundtrip() {
        let dir = std::env::temp_dir().join("lv-store-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("analyzers.lvcap");
        let mut w = CaptureWriter::create(
            &path,
            &NewCapture {
                device: "test".into(),
                samplerate_hz: 1_000_000,
                channel_names: (0..8).map(|i| format!("D{i}")).collect(),
            },
            true,
        )
        .unwrap();
        w.push(&[0u8; 1000]).unwrap();
        w.finish(Status::Complete).unwrap();

        let frames = vec![
            frame(10, 20, "H"),
            frame(30, 45, "i"),
            frame(100, 300, "긴 프레임"),
            frame(400, 410, "!"),
        ];
        let a = NewAnalyzer {
            kind: "sigrok".into(),
            decoder_id: "uart:rx=0".into(),
            label: "UART".into(),
            settings_json: "{}".into(),
            input_id: None,
        };
        let id = add_analyzer(&path, &a, &frames).unwrap();
        let hla = add_analyzer(
            &path,
            &NewAnalyzer {
                kind: "hla".into(),
                input_id: Some(id),
                ..a.clone()
            },
            &[frame(10, 45, "Hi")],
        )
        .unwrap();

        let f = CaptureFile::open(&path).unwrap();
        let list = f.analyzers().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].frames, list[0].max_span), (4, 200));
        // Overlap: the long frame starts before the window.
        let got = f.frames_in(id, 250, 405, list[0].max_span, 100).unwrap();
        assert_eq!(got, vec![frames[2].clone(), frames[3].clone()]);
        assert_eq!(f.frames_in(id, 0, 1000, 200, 2).unwrap().len(), 2);
        assert_eq!(f.frame_count(id, "").unwrap(), 4);
        assert_eq!(f.frame_rows(id).unwrap(), vec![0]);
        assert_eq!(f.frame_count(id, "프레임").unwrap(), 1);
        assert_eq!(f.frames_page(id, "", 1, 2).unwrap(), frames[1..3].to_vec());
        // AUT-4: read-only SQL with row and time limits.
        let q = f
            .query(
                "SELECT text, end_sample - start_sample AS len FROM frame WHERE analyzer_id = 1 ORDER BY start_sample",
                3,
                std::time::Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(q.columns, ["text", "len"]);
        assert_eq!(q.rows[0], [Cell::Text("H".into()), Cell::Int(10)]);
        assert!(q.truncated);
        assert!(
            f.query("DELETE FROM frame", 10, std::time::Duration::from_secs(2))
                .is_err()
        );
        assert!(
            f.query("SELEC 1", 10, std::time::Duration::from_secs(2))
                .unwrap_err()
                .contains("SQL")
        );
        let slow = f
            .query(
                "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n) SELECT count(*) FROM n",
                10,
                std::time::Duration::from_millis(200),
            )
            .unwrap_err();
        assert!(slow.contains("time limit"), "{slow}");
        drop(f);

        delete_analyzer(&path, id).unwrap();
        let f = CaptureFile::open(&path).unwrap();
        assert!(
            f.analyzers().unwrap().is_empty(),
            "stacked HLA {hla} removed too"
        );
    }
}
