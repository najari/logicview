//! App settings in `%LOCALAPPDATA%\logicview\app.db`: the last capture
//! settings (UI-10) and named decoder presets. `LOGICVIEW_APP_DB` points the
//! app at another file (tests). When the file cannot be opened the app runs
//! with defaults and remembers nothing.

use std::path::{Path, PathBuf};

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

/// Interface theme (`ui.theme`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [(Theme, &'static str); 3] = [
        (Theme::System, "시스템 따라"),
        (Theme::Light, "밝게"),
        (Theme::Dark, "어둡게"),
    ];

    fn key(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

/// Settings window: how the waveform is shown, and logging (`view.*`,
/// `ui.theme`, `log.*`).
#[derive(Debug, Clone, PartialEq)]
pub struct ViewSettings {
    pub theme: Theme,
    /// Keep the whole capture in view while recording.
    pub follow_live: bool,
    /// Show the whole capture when a recording ends.
    pub fit_on_stop: bool,
    /// Times count from the trigger.
    pub zero_at_trigger: bool,
    /// Dots at the samples when zoomed in past one sample per point.
    pub sample_dots: bool,
    /// Fill the area under high levels.
    pub fill_high: bool,
    /// `[r, g, b, a]` of that fill.
    pub fill_color: [u8; 4],
    /// A vertical line at the mouse cursor.
    pub cursor_line: bool,
    /// Markers snap to an edge this close (points).
    pub snap_px: f32,
    /// Height of a channel row in points; 0 = share the space.
    pub row_height: f32,
    pub log_level: String,
    pub log_lines: usize,
}

impl Default for ViewSettings {
    fn default() -> Self {
        ViewSettings {
            theme: Theme::Dark,
            follow_live: true,
            fit_on_stop: true,
            zero_at_trigger: false,
            sample_dots: false,
            fill_high: false,
            fill_color: [0x80, 0x80, 0xff, 0x30],
            cursor_line: true,
            snap_px: 8.0,
            row_height: 0.0,
            log_level: "warn".into(),
            log_lines: crate::applog::DEFAULT_LINES,
        }
    }
}

impl ViewSettings {
    pub fn load(db: &AppDb) -> ViewSettings {
        let mut v = ViewSettings::default();
        let flag = |k: &str, d: bool| db.get(k).map_or(d, |s| s == "true");
        if let Some(t) = db.get("ui.theme") {
            v.theme = Theme::ALL
                .iter()
                .find(|(t2, _)| t2.key() == t)
                .map_or(v.theme, |(t2, _)| *t2);
        }
        v.follow_live = flag("view.follow_live", v.follow_live);
        v.fit_on_stop = flag("view.fit_on_stop", v.fit_on_stop);
        v.zero_at_trigger = flag("view.zero_at_trigger", v.zero_at_trigger);
        v.sample_dots = flag("view.sample_dots", v.sample_dots);
        v.fill_high = flag("view.fill_high", v.fill_high);
        v.cursor_line = flag("view.cursor_line", v.cursor_line);
        if let Some(c) = db
            .get("view.fill_color")
            .and_then(|s| u32::from_str_radix(s.trim_start_matches('#'), 16).ok())
        {
            v.fill_color = c.to_be_bytes();
        }
        if let Some(px) = db.get("view.snap_px").and_then(|s| s.parse::<f32>().ok())
            && (0.0..=50.0).contains(&px)
        {
            v.snap_px = px;
        }
        if let Some(h) = db
            .get("view.row_height")
            .and_then(|s| s.parse::<f32>().ok())
            && (h == 0.0 || (16.0..=300.0).contains(&h))
        {
            v.row_height = h;
        }
        if let Some(l) = db
            .get("log.level")
            .filter(|l| crate::applog::parse_level(l).is_some())
        {
            v.log_level = l;
        }
        if let Some(n) = db.get("log.lines").and_then(|s| s.parse::<usize>().ok())
            && (100..=1_000_000).contains(&n)
        {
            v.log_lines = n;
        }
        v
    }

    pub fn save(&self, db: &AppDb) -> rusqlite::Result<()> {
        let b = |x: bool| x.to_string();
        db.set_all(&[
            ("ui.theme", self.theme.key().to_string()),
            ("view.follow_live", b(self.follow_live)),
            ("view.fit_on_stop", b(self.fit_on_stop)),
            ("view.zero_at_trigger", b(self.zero_at_trigger)),
            ("view.sample_dots", b(self.sample_dots)),
            ("view.fill_high", b(self.fill_high)),
            ("view.cursor_line", b(self.cursor_line)),
            (
                "view.fill_color",
                format!("{:08x}", u32::from_be_bytes(self.fill_color)),
            ),
            ("view.snap_px", self.snap_px.to_string()),
            ("view.row_height", self.row_height.to_string()),
            ("log.level", self.log_level.clone()),
            ("log.lines", self.log_lines.to_string()),
        ])
    }
}

/// Where decoders come from (`decode.runtime_dir`, `decode.extra_dirs`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecoderPaths {
    /// The decoder runtime folder; empty = look in the default places.
    pub runtime_dir: String,
    /// More folders with decoders (your own ones).
    pub extra_dirs: Vec<String>,
}

impl DecoderPaths {
    pub fn load(db: &AppDb) -> DecoderPaths {
        DecoderPaths {
            runtime_dir: db.get("decode.runtime_dir").unwrap_or_default(),
            extra_dirs: db
                .get("decode.extra_dirs")
                .and_then(|v| json_list(&v, |x| x.as_str().map(String::from)))
                .unwrap_or_default(),
        }
    }

    pub fn save(&self, db: &AppDb) -> rusqlite::Result<()> {
        db.set_all(&[
            ("decode.runtime_dir", self.runtime_dir.clone()),
            (
                "decode.extra_dirs",
                Value::from(self.extra_dirs.clone()).to_string(),
            ),
        ])
    }

    /// Hand the folders to the decoder runtime (used when it starts).
    pub fn apply(&self) {
        let dir = self.runtime_dir.trim();
        lv_decode::set_runtime_dir((!dir.is_empty()).then(|| PathBuf::from(dir)));
        for d in &self.extra_dirs {
            lv_decode::add_decoder_dir(Path::new(d));
        }
    }
}

/// Longest remembered capture length (seconds); anything longer is ignored.
const MAX_DURATION: f64 = 3600.0;

/// The capture settings the app remembers between runs (UI-10).
#[derive(Debug, Clone, PartialEq)]
pub struct Remembered {
    pub rate: u64,
    /// Capture length in seconds; `None` = until stopped.
    pub duration: Option<f64>,
    pub names: Vec<String>,
    pub shown: Vec<bool>,
    pub trigger_text: String,
    pub pre_percent: u8,
    /// Signal level in volts for devices with a threshold (WeAct U3).
    pub level: f64,
    /// Channels to capture where the device offers a choice (2, 4, 8, 16).
    pub channels: usize,
    /// Start and stop captures with the board key (WeAct U3).
    pub key_capture: bool,
    /// Indicator brightness, percent (WeAct U3).
    pub led: u8,
}

impl Remembered {
    /// `defaults` with whatever valid values `db` holds.
    pub fn load(db: &AppDb, defaults: Remembered) -> Remembered {
        let mut r = defaults;
        if let Some(v) = db.get("capture.rate").and_then(|v| v.parse().ok())
            && lv_device::all_samplerates().contains(&v)
        {
            r.rate = v;
        }
        // The length in seconds; 0.1.2 kept a sample count instead, which is
        // read as a length at the remembered rate.
        let length = match db.get("capture.duration") {
            Some(v) => Some(v),
            None => db.get("capture.samples").map(|v| {
                v.parse::<u64>()
                    .map_or(v, |n| (n as f64 / r.rate as f64).to_string())
            }),
        };
        match length.as_deref() {
            Some("manual") => r.duration = None,
            Some(v) => {
                if let Ok(s) = v.parse::<f64>()
                    && s.is_finite()
                    && s > 0.0
                    && s <= MAX_DURATION
                {
                    r.duration = Some(s);
                }
            }
            None => {}
        }
        // Up to 0.1.8 eight channels were kept: they are the first ones.
        if let Some(names) = db
            .get("channels.names")
            .and_then(|v| json_list(&v, |x| x.as_str().map(String::from)))
            && names.len() <= r.names.len()
        {
            r.names[..names.len()].clone_from_slice(&names);
        }
        if let Some(shown) = db
            .get("channels.shown")
            .and_then(|v| json_list(&v, Value::as_bool))
            && shown.len() <= r.shown.len()
        {
            r.shown[..shown.len()].copy_from_slice(&shown);
        }
        if let Some(t) = db.get("trigger.text") {
            r.trigger_text = t;
        }
        if let Some(p) = db.get("trigger.pre_percent").and_then(|v| v.parse().ok())
            && p <= 100
        {
            r.pre_percent = p;
        }
        if let Some(l) = db.get("capture.level").and_then(|v| v.parse::<f64>().ok())
            && (1.0..=3.3).contains(&l)
        {
            r.level = (l * 10.0).round() / 10.0;
        }
        if let Some(n) = db.get("capture.channels").and_then(|v| v.parse().ok())
            && matches!(n, 2 | 4 | 8 | 16)
        {
            r.channels = n;
        }
        if let Some(v) = db.get("device.key_capture") {
            r.key_capture = v == "true";
        }
        if let Some(v) = db.get("device.led").and_then(|v| v.parse().ok())
            && matches!(v, 10 | 20 | 40 | 70 | 100)
        {
            r.led = v;
        }
        r
    }

    pub fn save(&self, db: &AppDb) -> rusqlite::Result<()> {
        db.set_all(&[
            ("capture.rate", self.rate.to_string()),
            (
                "capture.duration",
                self.duration.map_or("manual".into(), |s| s.to_string()),
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
            ("capture.level", self.level.to_string()),
            ("capture.channels", self.channels.to_string()),
            ("device.key_capture", self.key_capture.to_string()),
            ("device.led", self.led.to_string()),
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
            duration: Some(1.0),
            names: (0..16).map(|i| format!("D{i}")).collect(),
            shown: vec![true; 16],
            trigger_text: "0=f".into(),
            pre_percent: 10,
            level: 3.3,
            channels: 16,
            key_capture: false,
            led: 20,
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
        r.duration = None;
        r.names[0] = "TX".into();
        r.names[3] = "한글".into();
        r.shown[7] = false;
        r.names[15] = "CS".into();
        r.channels = 4;
        r.level = 1.8;
        r.key_capture = true;
        r.led = 70;
        r.trigger_text = "0=r,1=1".into();
        r.pre_percent = 25;
        r.level = 1.8;
        r.save(&db).expect("save");
        drop(db);
        let db = AppDb::open(&path).expect("reopen");
        assert_eq!(Remembered::load(&db, defaults()), r);
        r.duration = Some(0.1);
        r.save(&db).expect("save again");
        assert_eq!(Remembered::load(&db, defaults()).duration, Some(0.1));
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ui10_sample_count_of_0_1_2_becomes_a_length() {
        let (path, db) = temp_db("old-samples");
        db.set_all(&[
            ("capture.rate", "4000000".into()),
            ("capture.samples", "2000000".into()),
        ])
        .expect("set");
        assert_eq!(Remembered::load(&db, defaults()).duration, Some(0.5));
        db.set_all(&[("capture.samples", "manual".into())])
            .expect("set");
        assert_eq!(Remembered::load(&db, defaults()).duration, None);
        // A length, once saved, wins over the old count.
        db.set_all(&[("capture.duration", "2".into())])
            .expect("set");
        assert_eq!(Remembered::load(&db, defaults()).duration, Some(2.0));
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn view_settings_round_trip() {
        let (path, db) = temp_db("view");
        assert_eq!(ViewSettings::load(&db), ViewSettings::default());
        let v = ViewSettings {
            theme: Theme::Light,
            follow_live: false,
            fit_on_stop: false,
            zero_at_trigger: true,
            sample_dots: true,
            fill_high: true,
            fill_color: [1, 2, 3, 4],
            cursor_line: false,
            snap_px: 12.0,
            row_height: 40.0,
            log_level: "debug".into(),
            log_lines: 300,
        };
        v.save(&db).expect("save");
        assert_eq!(ViewSettings::load(&db), v);
        db.set_all(&[
            ("view.row_height", "5".into()),
            ("log.level", "loud".into()),
            ("ui.theme", "pink".into()),
        ])
        .expect("set");
        let w = ViewSettings::load(&db);
        assert_eq!(
            (w.row_height, w.log_level.as_str(), w.theme),
            (0.0, "warn", Theme::Dark)
        );
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn decoder_paths_round_trip() {
        let (path, db) = temp_db("decoder-paths");
        assert_eq!(DecoderPaths::load(&db), DecoderPaths::default());
        let p = DecoderPaths {
            runtime_dir: r"C:\SLogicView".into(),
            extra_dirs: vec![r"D:\my decoders".into(), "E:/한글".into()],
        };
        p.save(&db).expect("save");
        drop(db);
        let db = AppDb::open(&path).expect("reopen");
        assert_eq!(DecoderPaths::load(&db), p);
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ch16_eight_saved_names_fill_the_front() {
        let (path, db) = temp_db("eight-names");
        let eight: Vec<String> = (0..8).map(|i| format!("N{i}")).collect();
        db.set_all(&[
            ("channels.names", serde_json::to_string(&eight).unwrap()),
            (
                "channels.shown",
                "[true,false,true,true,true,true,true,true]".into(),
            ),
        ])
        .expect("set");
        let r = Remembered::load(&db, defaults());
        assert_eq!(r.names[..8], eight[..]);
        assert_eq!(r.names[8], "D8");
        assert_eq!(r.names.len(), 16);
        assert!(!r.shown[1] && r.shown[15]);
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ui10_bad_values_keep_defaults() {
        let (path, db) = temp_db("bad");
        db.set_all(&[
            ("capture.rate", "12345".into()),
            ("capture.duration", "lots".into()),
            (
                "channels.names",
                serde_json::to_string(&vec!["x"; 17]).unwrap(),
            ),
            ("channels.shown", "not json".into()),
            ("trigger.pre_percent", "300".into()),
            ("capture.level", "3.7".into()),
            ("capture.channels", "12".into()),
            ("device.led", "50".into()),
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
