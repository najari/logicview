//! Tool table and input schemas, mirroring Logic 2's (and the Python server
//! in `mcp/logicview_mcp.py`), plus the argument validation with Logic 2's
//! coercions (numbers as strings, JSON objects as strings, ...).

use serde_json::{Map, Value, json};

fn num(desc: &str) -> Value {
    json!({"description": desc, "type": "number"})
}

fn channels(desc: &str) -> Value {
    json!({"type": "object", "description": desc, "properties": {
        "digitalChannels": {"description": "Digital channel indices (e.g. [0, 1, 2])", "type": "array",
                            "items": {"type": "number"}},
        "analogChannels": {"description": "Analog channel indices (not available on this device)", "type": "array",
                           "items": {"type": "number"}}}})
}

pub fn definitions() -> Value {
    json!({
        "GlitchFilter": {"type": "object", "properties": {
            "channelIndex": num("Digital channel index"),
            "pulseWidthSeconds": num("Minimum pulse width in seconds")},
            "required": ["channelIndex", "pulseWidthSeconds"]},
        "LinkedChannel": {"type": "object", "properties": {
            "channelIndex": num("Digital channel index"),
            "state": num("Required state: 1=low, 2=high")},
            "required": ["channelIndex", "state"]},
        "AnalyzerSettingValue": {"type": "object", "properties": {
            "stringValue": {"type": "string"}, "numberValue": {"type": "number"},
            "boolValue": {"type": "boolean"}}},
        "AnalyzerSettings": {"type": "object",
            "additionalProperties": {"$ref": "#/definitions/AnalyzerSettingValue"}},
        "AnalyzerExportConfig": {"type": "object", "properties": {
            "analyzerId": {"type": "number"}, "radixType": {"type": "number"}},
            "required": ["analyzerId"]},
    })
}

/// (name, description, properties, required)
pub fn tools() -> Vec<(&'static str, &'static str, Value, Vec<&'static str>)> {
    let capture_id = num("Capture ID");
    vec![
        (
            "get_devices",
            "List connected logic analyzers",
            json!({
            "includeSimulationDevices": {"description": "Include simulation devices in the response", "type": "boolean"}}),
            vec![],
        ),
        (
            "start_capture",
            "Start a new capture. Digital channels only; samplerates as listed in errors",
            json!({
            "deviceId": {"description": "Device id from get_devices (e.g. \"fx2lafw:usb/1-8\"). If omitted, the first device is used", "type": "string"},
            "logicDeviceConfiguration": {"description": "Hardware configuration", "type": "object", "properties": {
                "logicChannels": channels("Channels to enable"),
                "digitalSampleRate": num("Digital sample rate in samples/sec (e.g. 24000000)"),
                "analogSampleRate": num("Not supported"),
                "digitalThresholdVolts": num("Not supported (fixed threshold)"),
                "glitchFilters": {"description": "Not supported", "type": "array",
                                  "items": {"$ref": "#/definitions/GlitchFilter"}}}},
            "captureConfiguration": {"description": "Capture mode: one of manualCaptureMode, timedCaptureMode, digitalCaptureMode. Default: manual", "type": "object", "properties": {
                "bufferSizeMegabytes": num("Ignored"),
                "manualCaptureMode": {"description": "Capture until stop_capture", "type": "object", "properties": {
                    "trimDataSeconds": num("Keep only the last N seconds of data")}},
                "timedCaptureMode": {"description": "Capture for a fixed duration", "type": "object", "properties": {
                    "durationSeconds": num("Capture duration in seconds"),
                    "trimDataSeconds": num("Keep only the last N seconds of data")}},
                "digitalCaptureMode": {"description": "Capture on a digital edge trigger (10 % pre-trigger data)", "type": "object", "properties": {
                    "triggerType": num("1=rising, 2=falling (3/4 pulse triggers are not supported)"),
                    "afterTriggerSeconds": num("Seconds to capture after the trigger"),
                    "trimDataSeconds": num("Keep only the last N seconds of data"),
                    "triggerChannelIndex": num("Digital channel index to trigger on"),
                    "minPulseWidthSeconds": num("Not supported"),
                    "maxPulseWidthSeconds": num("Not supported"),
                    "linkedChannels": {"description": "Other channel levels required at the trigger", "type": "array",
                                       "items": {"$ref": "#/definitions/LinkedChannel"}}}}}}}),
            vec![],
        ),
        (
            "stop_capture",
            "Stop an active capture",
            json!({"captureId": num("Capture ID to stop")}),
            vec!["captureId"],
        ),
        (
            "wait_capture",
            "Wait for a capture to complete. Blocks until the capture finishes; a manual capture only finishes through stop_capture",
            json!({"captureId": num("Capture ID to wait for")}),
            vec!["captureId"],
        ),
        (
            "load_capture",
            "Load a capture from a sigrok session file (.sr, as saved by SLogicView/PulseView or save_capture) or a logicview .lvcap file",
            json!({"filepath": {"description": "Absolute path to the .sr or .lvcap file", "type": "string"}}),
            vec!["filepath"],
        ),
        (
            "save_capture",
            "Save a capture to a sigrok session file (.sr) or a logicview capture (.lvcap)",
            json!({
            "captureId": num("Capture ID to save"),
            "filepath": {"description": "Absolute path of the .sr or .lvcap file", "type": "string"}}),
            vec!["captureId", "filepath"],
        ),
        (
            "close_capture",
            "Close a capture and free resources",
            json!({"captureId": num("Capture ID to close")}),
            vec!["captureId"],
        ),
        (
            "add_analyzer",
            "Add a protocol analyzer to a capture and decode it",
            json!({
            "captureId": num("Capture ID to add analyzer to"),
            "analyzerName": {"description": "Logic 2 analyzer name (\"Async Serial\", \"I2C\", \"SPI\", \"CAN\") or any sigrok decoder id with logic input (e.g. \"onewire_link\", \"ir_nec\")", "type": "string"},
            "analyzerLabel": {"description": "User-facing label for the analyzer", "type": "string"},
            "settings": {"description": "Settings keyed by setting name (Logic 2 names; for sigrok decoders the channel/option ids). Channels take numberValue = channel index",
                         "$ref": "#/definitions/AnalyzerSettings"}}),
            vec!["analyzerName", "captureId"],
        ),
        (
            "remove_analyzer",
            "Remove a protocol analyzer from a capture",
            json!({
            "captureId": capture_id, "analyzerId": num("Analyzer ID to remove")}),
            vec!["analyzerId", "captureId"],
        ),
        (
            "add_high_level_analyzer",
            "Add a Saleae Logic 2 high-level analyzer extension (HLA) on top of an Async Serial, I2C, SPI or CAN analyzer",
            json!({
            "captureId": capture_id,
            "extensionDirectory": {"description": "Path to the extension directory (with extension.json)", "type": "string"},
            "hlaName": {"description": "Name of the HLA as listed in extension.json", "type": "string"},
            "hlaLabel": {"description": "User-facing label for the HLA", "type": "string"},
            "inputAnalyzerId": num("Analyzer ID to use as input"),
            "settings": {"description": "HLA settings keyed by setting label or attribute name",
                         "$ref": "#/definitions/AnalyzerSettings"}}),
            vec![
                "captureId",
                "extensionDirectory",
                "hlaName",
                "inputAnalyzerId",
            ],
        ),
        (
            "remove_high_level_analyzer",
            "Remove a high-level analyzer from a capture",
            json!({
            "captureId": capture_id, "analyzerId": num("HLA ID to remove")}),
            vec!["analyzerId", "captureId"],
        ),
        (
            "export_raw_data_csv",
            "Export raw digital data as CSV (digital.csv: one row per transition)",
            json!({
            "captureId": capture_id,
            "directory": {"description": "Directory to create exported CSV files in", "type": "string"},
            "logicChannels": channels("Channels to export. If omitted, all are exported."),
            "analogDownsampleRatio": num("Downsample ratio (1-1000000), kept for Logic 2 compatibility"),
            "iso8601Timestamp": {"description": "Use ISO8601 timestamps", "type": "boolean"}}),
            vec!["analogDownsampleRatio", "captureId", "directory"],
        ),
        (
            "export_raw_data_binary",
            "Export raw digital data in the Logic 2 binary format (digital_N.bin)",
            json!({
            "captureId": capture_id,
            "directory": {"description": "Directory to create exported binary files in", "type": "string"},
            "logicChannels": channels("Channels to export. If omitted, all are exported."),
            "analogDownsampleRatio": num("Downsample ratio (1-1000000), kept for Logic 2 compatibility")}),
            vec!["analogDownsampleRatio", "captureId", "directory"],
        ),
        (
            "export_data_table_csv",
            "Export analyzer results as a CSV data table (name, type, start_time, duration, value)",
            json!({
            "captureId": capture_id,
            "filepath": {"description": "Path to export the CSV to", "type": "string"},
            "analyzers": {"description": "Analyzers to include. If omitted, all are included", "type": "array",
                          "items": {"$ref": "#/definitions/AnalyzerExportConfig"}},
            "iso8601Timestamp": {"description": "Use ISO8601 timestamps", "type": "boolean"},
            "exportColumns": {"description": "Columns to export. If omitted, all are exported", "type": "array",
                              "items": {"type": "string"}},
            "filter": {"description": "Keep rows containing the query text (case-insensitive)", "type": "object",
                       "properties": {"query": {"type": "string"},
                                      "columns": {"type": "array", "items": {"type": "string"}}},
                       "required": ["query"]}}),
            vec!["captureId", "filepath"],
        ),
        (
            "legacy_export_analyzer",
            "Export one analyzer as CSV (Time [s], Type, Value)",
            json!({
            "captureId": capture_id,
            "filepath": {"description": "Path to export data to", "type": "string"},
            "analyzerId": num("Analyzer ID to export"),
            "radixType": num("Radix type (1=binary, 2=decimal, 3=hex, 4=ascii); applies to Async Serial")}),
            vec!["analyzerId", "captureId", "filepath", "radixType"],
        ),
        (
            "read_analyzer_frames",
            "logicview addition: return decoded frames of an analyzer or HLA directly",
            json!({
            "captureId": capture_id, "analyzerId": num("Analyzer or HLA ID"),
            "offset": num("First frame (default 0)"), "limit": num("Number of frames (default 200)")}),
            vec!["analyzerId", "captureId"],
        ),
        (
            "query_frames",
            "logicview addition: run one read-only SQL statement on the capture file (tables capture, channel, analyzer, frame, marker; at most 1000 rows, 2 s). frame(analyzer_id, start_sample, end_sample, ann_row, type, text, data); analyzer ids here are the file's, see the analyzer table",
            json!({
            "captureId": capture_id,
            "sql": {"description": "A SELECT statement, e.g. SELECT text, count(*) FROM frame WHERE type LIKE 'can.%' GROUP BY text", "type": "string"}}),
            vec!["captureId", "sql"],
        ),
    ]
}

pub fn tool_list() -> Value {
    let defs = definitions();
    Value::Array(
        tools()
            .into_iter()
            .map(|(name, desc, props, required)| {
                let mut schema =
                    json!({"type": "object", "properties": props, "definitions": defs});
                if !required.is_empty() {
                    schema["required"] = json!(required);
                }
                json!({"name": name, "description": desc, "inputSchema": schema})
            })
            .collect(),
    )
}

fn resolve(schema: &Value) -> Value {
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        let name = r.rsplit('/').next().unwrap_or_default();
        return definitions().get(name).cloned().unwrap_or(Value::Null);
    }
    schema.clone()
}

fn join(path: &str, key: &str) -> String {
    format!("{}.{key}", path.trim_end_matches('.'))
}

/// Validate and coerce `value` against `schema` (a JSON Schema subset).
pub fn validate(value: &Value, schema: &Value, path: &str) -> Result<Value, String> {
    let schema = resolve(schema);
    let kind = schema.get("type").and_then(Value::as_str).unwrap_or("");
    let mut value = value.clone();
    if matches!(kind, "object" | "array")
        && let Some(s) = value.as_str()
        && let Ok(v) = serde_json::from_str::<Value>(s)
    {
        value = v;
    }
    match kind {
        "number" => {
            let x = match &value {
                Value::String(s) => s
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| format!("{path}: should be number"))?,
                Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
                _ => return Err(format!("{path}: should be number")),
            };
            Ok(if x.fract() == 0.0 && x.abs() < 9e15 {
                json!(x as i64)
            } else {
                json!(x)
            })
        }
        "string" => match value {
            Value::String(_) => Ok(value),
            Value::Number(n) => Ok(Value::String(n.to_string())),
            _ => Err(format!("{path}: should be string")),
        },
        "boolean" => match &value {
            Value::Bool(_) => Ok(value),
            Value::String(s) if s.eq_ignore_ascii_case("true") => Ok(Value::Bool(true)),
            Value::String(s) if s.eq_ignore_ascii_case("false") => Ok(Value::Bool(false)),
            _ => Err(format!("{path}: should be boolean")),
        },
        "array" => {
            let Value::Array(items) = value else {
                return Err(format!("{path}: should be array"));
            };
            let item_schema = schema.get("items").cloned().unwrap_or(json!({}));
            items
                .iter()
                .enumerate()
                .map(|(i, v)| validate(v, &item_schema, &format!("{path}[{i}]")))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        }
        "object" => {
            let Value::Object(map) = value else {
                return Err(format!("{path}: should be object"));
            };
            if let Some(req) = schema.get("required").and_then(Value::as_array) {
                for k in req.iter().filter_map(Value::as_str) {
                    if !map.contains_key(k) {
                        return Err(format!("{path}: should have required property '{k}'"));
                    }
                }
            }
            let props = schema.get("properties").cloned().unwrap_or(json!({}));
            let extra = schema.get("additionalProperties").filter(|e| e.is_object());
            let mut out = Map::new();
            for (k, v) in map {
                let v = if let Some(p) = props.get(&k) {
                    validate(&v, p, &join(path, &k))?
                } else if let Some(e) = extra {
                    validate(&v, e, &join(path, &k))?
                } else {
                    v
                };
                out.insert(k, v);
            }
            Ok(Value::Object(out))
        }
        _ => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_of(tool: &str) -> Value {
        let (_, _, props, required) = tools().into_iter().find(|t| t.0 == tool).unwrap();
        json!({"type": "object", "properties": props, "required": required})
    }

    #[test]
    fn aut3_tool_table() {
        let list = tool_list();
        let names: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 17);
        assert!(names.contains(&"read_analyzer_frames") && names.contains(&"query_frames"));
        assert!(list[1]["inputSchema"]["definitions"]["LinkedChannel"].is_object());
    }

    #[test]
    fn aut3_coercions() {
        let s = schema_of("add_analyzer");
        let v = validate(
            &json!({"captureId": "3", "analyzerName": "I2C", "settings": "{\"SDA\": {\"numberValue\": \"2\"}}"}),
            &s,
            ".",
        )
        .unwrap();
        assert_eq!(v["captureId"], json!(3));
        assert_eq!(v["settings"]["SDA"]["numberValue"], json!(2));
        assert_eq!(
            validate(&json!({"analyzerName": "x"}), &s, ".").unwrap_err(),
            ".: should have required property 'captureId'"
        );
        assert_eq!(
            validate(&json!({"captureId": "x", "analyzerName": "I2C"}), &s, ".").unwrap_err(),
            ".captureId: should be number"
        );
        let e = schema_of("export_raw_data_csv");
        let v = validate(
            &json!({"captureId": 1, "directory": 5, "analogDownsampleRatio": 1.5, "iso8601Timestamp": "TRUE"}),
            &e,
            ".",
        )
        .unwrap();
        assert_eq!(v["directory"], json!("5"));
        assert_eq!(v["analogDownsampleRatio"], json!(1.5));
        assert_eq!(v["iso8601Timestamp"], json!(true));
    }
}
