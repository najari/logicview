//! `.lvcap` schema, version 1 (PRD section 5).

pub const FORMAT: &str = "lvcap";
pub const VERSION: i64 = 1;

pub const SCHEMA: &str = r#"
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE capture (
  id INTEGER PRIMARY KEY,
  created_at TEXT NOT NULL,
  device TEXT NOT NULL,
  samplerate_hz INTEGER NOT NULL,
  num_channels INTEGER NOT NULL,
  num_samples INTEGER NOT NULL,
  trigger_json TEXT,
  status TEXT NOT NULL
);

CREATE TABLE channel (
  capture_id INTEGER NOT NULL REFERENCES capture(id),
  idx INTEGER NOT NULL,
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (capture_id, idx)
);

CREATE TABLE sample_chunk (
  capture_id INTEGER NOT NULL REFERENCES capture(id),
  chunk_idx INTEGER NOT NULL,
  first_sample INTEGER NOT NULL,
  n_samples INTEGER NOT NULL,
  codec TEXT NOT NULL,
  data BLOB NOT NULL,
  PRIMARY KEY (capture_id, chunk_idx)
);

CREATE TABLE summary (
  capture_id INTEGER NOT NULL REFERENCES capture(id),
  level INTEGER NOT NULL,
  chunk_idx INTEGER NOT NULL,
  data BLOB NOT NULL,
  PRIMARY KEY (capture_id, level, chunk_idx)
);

CREATE TABLE analyzer (
  id INTEGER PRIMARY KEY,
  capture_id INTEGER NOT NULL REFERENCES capture(id),
  kind TEXT NOT NULL,
  decoder_id TEXT NOT NULL,
  label TEXT NOT NULL,
  settings_json TEXT NOT NULL,
  input_id INTEGER REFERENCES analyzer(id)
);

CREATE TABLE frame (
  id INTEGER PRIMARY KEY,
  analyzer_id INTEGER NOT NULL REFERENCES analyzer(id),
  start_sample INTEGER NOT NULL,
  end_sample INTEGER NOT NULL,
  ann_row INTEGER NOT NULL,
  type TEXT NOT NULL,
  text TEXT NOT NULL,
  data TEXT
);
CREATE INDEX frame_by_time ON frame(analyzer_id, start_sample);

CREATE TABLE marker (
  capture_id INTEGER NOT NULL REFERENCES capture(id),
  sample INTEGER NOT NULL,
  label TEXT NOT NULL
);
"#;

/// UTC time as RFC 3339 (`2026-11-10T04:30:00Z`) without a date crate.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
