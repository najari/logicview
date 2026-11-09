//! JSON-RPC handling and the transports: Streamable HTTP with JSON
//! responses (like Logic 2, default http://127.0.0.1:10530) and stdio
//! (AUT-5). Each request runs on its own thread, since tools such as
//! `wait_capture` block.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::schema;
use crate::tools::Server;

pub const SERVER_NAME: &str = "logicview";
pub const PROTOCOL_VERSION: &str = "2025-03-26";

fn reply(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

/// Answer one JSON-RPC request; `None` for notifications.
pub fn handle(server: &Server, req: &Value) -> Option<Value> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str);
    let (Some(method), true) = (
        method,
        req.get("jsonrpc").and_then(Value::as_str) == Some("2.0"),
    ) else {
        return Some(error(&id, -32600, "Invalid Request"));
    };
    let params = req.get("params").cloned().unwrap_or(json!({}));
    match method {
        "initialize" => Some(reply(
            &id,
            json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {"tools": {}},
                   "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")}}),
        )),
        "ping" => Some(reply(&id, json!({}))),
        "tools/list" => Some(reply(&id, json!({"tools": schema::tool_list()}))),
        "tools/call" => {
            let Some(name) = params
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
            else {
                return Some(error(&id, -32602, "Missing tool name"));
            };
            let Some((_, _, props, required)) = schema::tools().into_iter().find(|t| t.0 == name)
            else {
                return Some(error(&id, -32601, format!("Unknown tool: {name}")));
            };
            let raw = params
                .get("arguments")
                .filter(|a| !a.is_null())
                .cloned()
                .unwrap_or(json!({}));
            let args = match schema::validate(
                &raw,
                &json!({"type": "object", "properties": props, "required": required}),
                ".",
            ) {
                Ok(a) => a,
                Err(e) => return Some(error(&id, -32602, e)),
            };
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server.call(name, &args)));
            Some(reply(
                &id,
                match result {
                    Ok(Ok(v)) => json!({"content": [{"type": "text", "text": v.to_string()}],
                                        "structuredContent": v, "isError": false}),
                    Ok(Err(e)) => {
                        json!({"content": [{"type": "text", "text": format!("[{}] {}", e.code, e.message)}],
                                         "isError": true})
                    }
                    Err(_) => {
                        log::error!("tool {name} panicked");
                        json!({"content": [{"type": "text", "text": format!("[INTERNAL_ERROR] tool {name} failed")}],
                               "isError": true})
                    }
                },
            ))
        }
        _ if id.is_null() => None,
        _ => Some(error(&id, -32601, format!("Unknown method: {method}"))),
    }
}

/// Parse and answer one request body.
pub fn handle_text(server: &Server, body: &str) -> Option<String> {
    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Some(error(&Value::Null, -32700, "Parse error").to_string()),
    };
    handle(server, &req).map(|v| v.to_string())
}

// ---- HTTP ------------------------------------------------------------------------------

fn respond(stream: &mut TcpStream, status: &str, body: Option<&[u8]>) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type, Accept, Mcp-Session-Id, Mcp-Protocol-Version\r\nConnection: close\r\n"
    );
    match body {
        Some(b) => head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            b.len()
        )),
        None => head.push_str("Content-Length: 0\r\n\r\n"),
    }
    stream.write_all(head.as_bytes())?;
    if let Some(b) = body {
        stream.write_all(b)?;
    }
    stream.flush()
}

fn serve_conn(server: &Server, stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let method = line.split_whitespace().next().unwrap_or("").to_string();
    let mut length = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut stream = stream;
    match method.as_str() {
        "OPTIONS" => respond(&mut stream, "204 No Content", None),
        "POST" => {
            let mut body = vec![0u8; length.min(64 << 20)];
            reader.read_exact(&mut body)?;
            match handle_text(server, &String::from_utf8_lossy(&body)) {
                Some(r) => respond(&mut stream, "200 OK", Some(r.as_bytes())),
                None => respond(&mut stream, "202 Accepted", None),
            }
        }
        _ => respond(
            &mut stream,
            "405 Method Not Allowed",
            Some(br#"{"error":"Method not allowed"}"#),
        ),
    }
}

/// A running HTTP server; dropping it does not stop it, [`HttpHandle::stop`] does.
pub struct HttpHandle {
    stop: Arc<AtomicBool>,
    pub addr: std::net::SocketAddr,
}

impl HttpHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Listen on `host:port` in a background thread.
pub fn start_http(server: Arc<Server>, host: &str, port: u16) -> std::io::Result<HttpHandle> {
    let listener = TcpListener::bind((host, port))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::Builder::new()
        .name("mcp-http".into())
        .spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let server = server.clone();
                        std::thread::spawn(move || {
                            if let Err(e) = serve_conn(&server, stream) {
                                log::debug!("mcp http: {e}");
                            }
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(30));
                    }
                    Err(e) => {
                        log::warn!("mcp http accept: {e}");
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        })?;
    log::info!("MCP HTTP server listening on http://{addr}");
    Ok(HttpHandle { stop, addr })
}

/// Speak MCP over stdin/stdout until stdin closes (AUT-5).
pub fn serve_stdio(server: Arc<Server>) {
    // Start the embedded Python before stdin is being read: on Windows a
    // thread blocked reading a synchronous pipe blocks every other call on
    // that handle, and Python's start-up inspects stdin.
    if let Err(e) = lv_decode::runtime() {
        log::warn!("decoder runtime: {e}");
    }
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let server = server.clone();
        let out = out.clone();
        std::thread::spawn(move || {
            if let Some(r) = handle_text(&server, &line)
                && let Ok(mut o) = out.lock()
            {
                let _ = writeln!(o, "{r}");
                let _ = o.flush();
            }
        });
    }
}
