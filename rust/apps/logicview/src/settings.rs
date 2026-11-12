//! App settings in `%LOCALAPPDATA%\logicview\app.db`: the last capture
//! settings (UI-10) and named decoder presets. `LOGICVIEW_APP_DB` points the
//! app at another file (tests). When the file cannot be opened the app runs
//! with defaults and remembers nothing.

use std::path::{Path, PathBuf};

use lv_fx2lafw::SAMPLERATES;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

pub fn default_path() -> PathBuf {
    if let Some(p) = std::env::var_os("LOGICVIEW_APP_DB") {
        return PathBuf::from(p);
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("logicview")
        .join("app.db")
}

pub struct AppDb {
    conn: Option<Connection>,
}

impl AppDb {
    pub fn open(path: &Path) -> rusqlite::Result<AppDb> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS setting (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS decoder_preset (name TEXT PRIMARY KEY, stack TEXT NOT NULL);",
        )?;
        Ok(AppDb { conn: Some(conn) })
    }

    pub fn open_default() -> AppDb {
        let path = default_path();
        Self::open(&path).unwrap_or_else(|e| {
            log::warn!(
                "settings file {}: {e}; settings are not remembered",
                path.display()
            );
            AppDb { conn: None }
        })
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let conn = self.conn.as_ref()?;
        conn.query_row("SELECT value FROM setting WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()
        .unwrap_or_else(|e| {
            log::warn!("reading setting {key}: {e}");
            None
        })
    }

    pub fn set_all(&self, pairs: &[(&str, String)]) -> rusqlite::Result<()> {
        let Some(conn) = &self.conn else {
            return Ok(());
        };
        let tx = conn.unchecked_transaction()?;
        for (k, v) in pairs {
            tx.execute(
                "INSERT INTO setting (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![k, v],
            )?;
        }
        tx.commit()
    }

    /// (name, stack JSON) by name.
    pub fn presets(&self) -> Vec<(String, String)> {
        let Some(conn) = &self.conn else {
            return Vec::new();
        };
        let read = || -> rusqlite::Result<Vec<(String, String)>> {
            let mut st = conn.prepare("SELECT name, stack FROM decoder_preset ORDER BY name")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
        };
        read().unwrap_or_else(|e| {
            log::warn!("reading decoder presets: {e}");
            Vec::new()
        })
    }

    pub fn save_preset(&self, name: &str, stack: &str) -> Result<(), String> {
        let conn = self
            .conn
            .as_ref()
            .ok_or("설정 파일을 열 수 없어 저장하지 못했습니다")?;
        conn.execute(
            "INSERT INTO decoder_preset (name, stack) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET stack = excluded.stack",
            params![name, stack],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    pub fn delete_preset(&self, name: &str) -> Result<(), String> {
        let conn = self.conn.as_ref().ok_or("설정 파일을 열 수 없습니다")?;
        conn.execute("DELETE FROM decoder_preset WHERE name = ?1", [name])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// The capture settings the app remembers between runs (UI-10).
#[derive(Debug, Clone, PartialEq)]
pub struct Remembered {
    pub rate: u64,
    /// `None` = until stopped.
    pub samples: Option<u64>,
    pub names: Vec<String>,
    pub shown: Vec<bool>,
    pub trigger_text: String,
    pub pre_percent: u8,
}

impl Remembered {
    /// `defaults` with whatever valid values `db` holds.
    pub fn load(db: &AppDb, defaults: Remembered) -> Remembered {
        let mut r = defaults;
        if let Some(v) = db.get("capture.rate").and_then(|v| v.parse().ok())
            && SAMPLERATES.contains(&v)
        {
            r.rate = v;
        }
        match db.get("capture.samples").as_deref() {
            Some("manual") => r.samples = None,
            Some(v) => {
                if let Ok(n) = v.parse::<u64>()
                    && n > 0
                {
                    r.samples = Some(n);
                }
            }
            None => {}
        }
        if let Some(names) = db
            .get("channels.names")
            .and_then(|v| json_list(&v, |x| x.as_str().map(String::from)))
            && names.len() == r.names.len()
        {
            r.names = names;
        }
        if let Some(shown) = db
            .get("channels.shown")
            .and_then(|v| json_list(&v, Value::as_bool))
            && shown.len() == r.shown.len()
        {
            r.shown = shown;
        }
        if let Some(t) = db.get("trigger.text") {
            r.trigger_text = t;
        }
        if let Some(p) = db.get("trigger.pre_percent").and_then(|v| v.parse().ok())
            && p <= 100
        {
            r.pre_percent = p;
        }
        r
    }

    pub fn save(&self, db: &AppDb) -> rusqlite::Result<()> {
        db.set_all(&[
            ("capture.rate", self.rate.to_string()),
            (
                "capture.samples",
                self.samples.map_or("manual".into(), |n| n.to_string()),
            ),
            (
                "channels.names",
                Value::from(self.names.clone()).to_string(),
            ),
            (
                "channels.shown",
                Value::from(self.shown.clone()).to_string(),
            ),
            ("trigger.text", self.trigger_text.clone()),
            ("trigger.pre_percent", self.pre_percent.to_string()),
        ])
    }
}

fn json_list<T>(text: &str, item: impl Fn(&Value) -> Option<T>) -> Option<Vec<T>> {
    let v: Value = serde_json::from_str(text).ok()?;
    v.as_array()?.iter().map(item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Remembered {
        Remembered {
            rate: 16_000_000,
            samples: Some(10_000_000),
            names: (0..8).map(|i| format!("D{i}")).collect(),
            shown: vec![true; 8],
            trigger_text: "0=f".into(),
            pre_percent: 10,
        }
    }

    fn temp_db(name: &str) -> (PathBuf, AppDb) {
        let path =
            std::env::temp_dir().join(format!("logicview-test-{name}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = AppDb::open(&path).expect("open");
        (path, db)
    }

    #[test]
    fn ui10_settings_round_trip() {
        let (path, db) = temp_db("settings");
        assert_eq!(
            Remembered::load(&db, defaults()),
            defaults(),
            "empty file: defaults"
        );
        let mut r = defaults();
        r.rate = 1_000_000;
        r.samples = None;
        r.names[0] = "TX".into();
        r.names[3] = "한글".into();
        r.shown[7] = false;
        r.trigger_text = "0=r,1=1".into();
        r.pre_percent = 25;
        r.save(&db).expect("save");
        drop(db);
        let db = AppDb::open(&path).expect("reopen");
        assert_eq!(Remembered::load(&db, defaults()), r);
        r.samples = Some(50_000_000);
        r.save(&db).expect("save again");
        assert_eq!(Remembered::load(&db, defaults()).samples, Some(50_000_000));
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ui10_bad_values_keep_defaults() {
        let (path, db) = temp_db("bad");
        db.set_all(&[
            ("capture.rate", "12345".into()),
            ("capture.samples", "lots".into()),
            ("channels.names", "[\"only one\"]".into()),
            ("channels.shown", "not json".into()),
            ("trigger.pre_percent", "300".into()),
        ])
        .expect("set");
        assert_eq!(Remembered::load(&db, defaults()), defaults());
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn presets_save_replace_delete() {
        let (path, db) = temp_db("presets");
        db.save_preset("UART 115200", "{\"a\":1}").expect("save");
        db.save_preset("I2C", "{\"b\":2}").expect("save");
        db.save_preset("UART 115200", "{\"a\":3}").expect("replace");
        assert_eq!(
            db.presets(),
            vec![
                ("I2C".to_string(), "{\"b\":2}".to_string()),
                ("UART 115200".to_string(), "{\"a\":3}".to_string()),
            ]
        );
        db.delete_preset("I2C").expect("delete");
        assert_eq!(db.presets().len(), 1);
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn no_file_means_defaults() {
        let db = AppDb { conn: None };
        assert_eq!(Remembered::load(&db, defaults()), defaults());
        assert!(db.save_preset("x", "{}").is_err());
        assert!(db.presets().is_empty());
        defaults().save(&db).expect("no-op");
    }
}
