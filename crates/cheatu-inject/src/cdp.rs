//! A minimal Chrome DevTools Protocol client over a WebSocket.
//!
//! Enough of CDP to evaluate JavaScript in a page and read the result — which
//! is all the JS-injection backend needs.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// A live CDP session bound to one page target.
pub struct Cdp {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Cdp {
    /// Connect to the DevTools endpoint on `127.0.0.1:<port>` and attach to the
    /// game's page target.
    pub fn connect(port: u16) -> Result<Cdp, String> {
        let ws_url = page_target(port)?;
        let (socket, _resp) =
            tungstenite::connect(&ws_url).map_err(|e| format!("websocket connect: {e}"))?;
        let mut cdp = Cdp { socket, next_id: 1 };
        // Best-effort; ignore the ack.
        let _ = cdp.call("Runtime.enable", json!({}));
        Ok(cdp)
    }

    /// Evaluate a JavaScript expression in the page and return its value.
    ///
    /// `returnByValue` gives us a plain JSON value; `awaitPromise` lets the
    /// expression be async if needed.
    pub fn eval(&mut self, expression: &str) -> Result<Value, String> {
        let result = self.call(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
                "replMode": true,
            }),
        )?;

        if let Some(exc) = result.get("exceptionDetails") {
            let text = exc
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
                .or_else(|| exc.get("text").and_then(|t| t.as_str()))
                .unwrap_or("JavaScript error");
            return Err(text.to_string());
        }
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub fn eval_f64(&mut self, expression: &str) -> Result<f64, String> {
        self.eval(expression)?
            .as_f64()
            .ok_or_else(|| "expected a number".to_string())
    }

    pub fn eval_bool(&mut self, expression: &str) -> Result<bool, String> {
        Ok(self.eval(expression)?.as_bool().unwrap_or(false))
    }

    pub fn eval_string(&mut self, expression: &str) -> Result<String, String> {
        Ok(self
            .eval(expression)?
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// Send a CDP command and wait for the matching response, skipping events.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({ "id": id, "method": method, "params": params });
        self.socket
            .send(Message::Text(msg.to_string()))
            .map_err(|e| format!("send: {e}"))?;

        loop {
            let msg = self.socket.read().map_err(|e| format!("read: {e}"))?;
            let text = match msg {
                Message::Text(t) => t,
                Message::Ping(p) => {
                    let _ = self.socket.send(Message::Pong(p));
                    continue;
                }
                Message::Close(_) => return Err("connection closed".into()),
                _ => continue,
            };
            let v: Value = serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(err.to_string());
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            // Otherwise it's a protocol event; ignore and keep reading.
        }
    }
}

/// Query the DevTools HTTP endpoint and return the page target's WebSocket URL.
fn page_target(port: u16) -> Result<String, String> {
    let body = http_get(port, "/json/list")
        .or_else(|_| http_get(port, "/json"))
        .map_err(|e| format!("no DevTools endpoint on 127.0.0.1:{port} ({e}) — is the game running with --remote-debugging-port?"))?;

    let targets: Value =
        serde_json::from_str(&body).map_err(|e| format!("bad /json response: {e}"))?;
    let arr = targets.as_array().ok_or("unexpected /json response")?;

    // Prefer the main page (index.html); fall back to any page with a ws URL.
    let mut fallback: Option<String> = None;
    for t in arr {
        if t.get("type").and_then(|v| v.as_str()) != Some("page") {
            continue;
        }
        let Some(ws) = t.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) else {
            continue;
        };
        let url = t.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.contains("index.html") {
            return Ok(ws.to_string());
        }
        fallback.get_or_insert_with(|| ws.to_string());
    }
    fallback.ok_or_else(|| "no debuggable page target found".to_string())
}

/// Minimal HTTP GET returning the response body (DevTools JSON endpoint).
fn http_get(port: u16, path: &str) -> Result<String, String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("connect: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;

    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(_) => break, // timeout or reset: use what we have
        }
    }
    let text = String::from_utf8_lossy(&raw);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    if body.trim().is_empty() {
        return Err("empty response".into());
    }
    Ok(body)
}
