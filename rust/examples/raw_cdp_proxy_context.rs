//! The whole handshake over a raw DevTools WebSocket, with no driver at all.
//!
//! ```sh
//! # 1. run Chromeleon with CDP exposed (see examples/launch_and_attach.rs)
//! # 2. take its browser WebSocket URL
//! ws=$(curl -s http://127.0.0.1:9222/json/version | grep -o 'ws://[^"]*')
//! cargo run --example raw_cdp_proxy_context -- "$ws" 'http://user:pass@gateway:12321'
//! ```
//!
//! This is the smallest complete thing: 30 lines of WebSocket plumbing, one call
//! into this crate, and a browser context that egresses through an
//! authenticated proxy. Every other adapter is the same two commands with a
//! driver's `send` instead of this one.

use std::cell::RefCell;

use chromeleon::adapters::new_proxy_context_raw_blocking;
use serde_json::{json, Value};
use tungstenite::Message;

type Error = Box<dyn std::error::Error>;

fn main() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let ws_url = args.next().ok_or("usage: <browser-ws-url> <proxy-url>")?;
    let proxy = args.next().ok_or("usage: <browser-ws-url> <proxy-url>")?;

    let (socket, _response) = tungstenite::connect(&ws_url)?;
    let cdp = Cdp {
        socket: RefCell::new(socket),
        next_id: RefCell::new(1),
    };

    // The handshake. `new_proxy_context_raw_blocking` holds the registration
    // slot across both commands, sends setProxyCredentials with the decoded
    // credentials, and creates the context with the same normalized server
    // string — which is what consumes the registration.
    let created = new_proxy_context_raw_blocking(
        // The connection's identity, so two contexts on this socket asking for
        // the same proxy queue instead of racing.
        ws_url.as_str(),
        proxy.as_str(),
        |method, params| cdp.call(method, params),
    )?;

    let context_id = created["result"]["browserContextId"]
        .as_str()
        .or_else(|| created["browserContextId"].as_str())
        .ok_or_else(|| format!("no browserContextId in {created}"))?;
    println!("browserContextId: {context_id}");

    // Prove it: open a page in that context and read back the egress IP.
    let target = cdp.call(
        "Target.createTarget",
        json!({"url": "https://api.ipify.org?format=json", "browserContextId": context_id}),
    )?;
    println!("targetId: {}", target["result"]["targetId"]);
    println!("the page is loading through the proxy; attach a session to read it back");
    Ok(())
}

/// A CDP connection: send a command, wait for the reply with the matching id.
struct Cdp {
    socket:
        RefCell<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>>,
    next_id: RefCell<u64>,
}

impl Cdp {
    fn call(&self, method: &str, params: Value) -> Result<Value, Error> {
        let id = {
            let mut next = self.next_id.borrow_mut();
            *next += 1;
            *next
        };
        let mut socket = self.socket.borrow_mut();
        socket.send(Message::Text(
            json!({"id": id, "method": method, "params": params})
                .to_string()
                .into(),
        ))?;
        loop {
            let message = socket.read()?;
            let Message::Text(text) = message else {
                continue;
            };
            let value: Value = serde_json::from_str(&text)?;
            // Events have no id; replies to earlier commands are not ours.
            if value["id"].as_u64() == Some(id) {
                return Ok(value);
            }
        }
    }
}
