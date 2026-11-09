//! Decoding through the real libsigrokdecode runtime. Skipped (passes with a
//! note) when no runtime is found; set LOGICVIEW_SIGROK_DIR to run it.

use lv_decode::{Control, parse_stack, stack_rows};
use lv_store::{CaptureFile, CaptureWriter, NewCapture, Status};

/// 1 Msample/s, UART 115200 8N1 on D0 (idle high), D1 named "TX".
fn uart_capture(name: &str, bytes: &[u8]) -> CaptureFile {
    let rate = 1_000_000.0f64;
    let bit = rate / 115_200.0;
    let mut samples = vec![1u8; 2000];
    for &b in bytes {
        let frame: Vec<u8> = std::iter::once(0)
            .chain((0..8).map(|k| (b >> k) & 1))
            .chain(std::iter::once(1))
            .collect();
        let start = samples.len();
        let n = (10.0 * bit).ceil() as usize + 20;
        for i in 0..n {
            let idx = ((i as f64) / bit) as usize;
            samples.push(*frame.get(idx).unwrap_or(&1));
        }
        assert!(samples.len() > start);
    }
    samples.extend(std::iter::repeat_n(1u8, 2000));
    let dir = std::env::temp_dir().join("lv-decode-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let mut names: Vec<String> = (0..8).map(|i| format!("D{i}")).collect();
    names[1] = "TX".into();
    let mut w = CaptureWriter::create(
        &path,
        &NewCapture {
            device: "test".into(),
            samplerate_hz: rate as u64,
            channel_names: names,
        },
        true,
    )
    .unwrap();
    w.push(&samples).unwrap();
    w.finish(Status::Complete).unwrap();
    CaptureFile::open(&path).unwrap()
}

#[test]
fn dec1_uart_bytes_through_libsigrokdecode() {
    let rt = match lv_decode::runtime() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let f = uart_capture("uart.lvcap", b"OK\n");
    let stack = parse_stack("uart:rx=D0:format=ascii").unwrap();
    let d = rt.decode(&f, &stack, &Control::default()).unwrap();
    let uart = &d.stack[0];
    let data: Vec<&str> = d
        .annotations
        .iter()
        .filter(|a| uart.class_id(a.class) == "rx-data")
        .map(|a| a.texts[0].as_str())
        .collect();
    assert_eq!(data, ["O", "K", "[0A]"]);
    assert_eq!(d.proto_ids, ["uart-1"]);

    // Stored rows point into the stack's row list.
    let (settings, frames) = d.to_frames(&stack);
    let (rows, _) = stack_rows(&d.stack);
    assert_eq!(settings.rows, rows);
    assert!(frames.iter().all(|f| (f.row as usize) < rows.len()));
    assert_eq!(settings.stack, "uart:rx=D0:format=ascii");

    // Errors name what is wrong.
    let bad = rt
        .decode(&f, &parse_stack("uart:rx=D9").unwrap(), &Control::default())
        .unwrap_err()
        .to_string();
    assert!(bad.contains("D9"), "{bad}");
    let bad = rt
        .decode(
            &f,
            &parse_stack("uart:speed=1").unwrap(),
            &Control::default(),
        )
        .unwrap_err()
        .to_string();
    assert!(bad.contains("speed") && bad.contains("baudrate"), "{bad}");
    let bad = rt
        .decode(
            &f,
            &parse_stack("nosuchdecoder").unwrap(),
            &Control::default(),
        )
        .unwrap_err()
        .to_string();
    assert!(bad.contains("nosuchdecoder"), "{bad}");
    // Channel by name.
    let d = rt
        .decode(&f, &parse_stack("uart:tx=TX").unwrap(), &Control::default())
        .unwrap();
    assert!(d.annotations.is_empty(), "D1 is idle");
}
