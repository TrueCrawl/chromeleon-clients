//! Wait for a page to actually finish, over a raw DevTools WebSocket.
//!
//! ```sh
//! # Chromeleon must have been launched with --page-settle (Launcher::page_settle(true)),
//! # or this demonstrates the networkAlmostIdle fallback instead — which is the point.
//! ws=$(curl -s http://127.0.0.1:9222/json/version | grep -o 'ws://[^"]*')
//! cargo run --example settle_wait -- "$ws" https://example.com
//! ```
//!
//! The shape is the whole lesson: the watch is armed on the page session
//! **before** `Page.navigate`. A session attached to an already-committed
//! document cannot see the challenge header the browser records at commit time,
//! and silently degrades to status-code-only detection.

use std::collections::VecDeque;
use std::time::Duration;

use chromeleon::settle::{BlockingPageSession, BlockingSettleWatch, WaitOptions};
use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

type Error = Box<dyn std::error::Error>;

fn main() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let ws_url = args.next().ok_or("usage: <browser-ws-url> <page-url>")?;
    let page_url = args
        .next()
        .unwrap_or_else(|| "https://example.com".to_string());

    let (mut socket, _) = tungstenite::connect(&ws_url)?;
    // A blocking transport with no read timeout can hang forever; the settle
    // watch bounds what it can, and this bounds the rest.
    set_read_timeout(&mut socket, Duration::from_secs(30))?;

    let mut cdp = Cdp {
        socket,
        next_id: 0,
        session_id: None,
        events: VecDeque::new(),
    };

    // A blank target first, so the watch can be armed before anything commits.
    let target = cdp.call("Target.createTarget", json!({"url": "about:blank"}), None)?;
    let target_id = target["result"]["targetId"]
        .as_str()
        .ok_or("no targetId")?
        .to_string();
    // Flat mode: replies and events carry a sessionId instead of being wrapped.
    let attached = cdp.call(
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        None,
    )?;
    cdp.session_id = Some(
        attached["result"]["sessionId"]
            .as_str()
            .ok_or("no sessionId")?
            .to_string(),
    );

    // 1. ARM — Page.enable, the main frame id, the lifecycle feed, and 350ms of
    //    reading to write off the about:blank's replayed lifecycle.
    let mut watch = BlockingSettleWatch::arm(cdp)?;
    println!("armed on frame {}", watch.main_frame_id().unwrap_or("?"));

    // 2. NAVIGATE — committing is enough; the watch is already recording.
    watch
        .session_mut()
        .call("Page.navigate", json!({ "url": page_url }), None)?;

    // 3. WAIT — the browser's own command, or the lifecycle fallback if this
    //    binary was launched without --page-settle. Never raises because a page
    //    did not settle: on proxied traffic that is a normal outcome.
    let state = watch.wait(WaitOptions::new().timeout_ms(30_000))?;
    println!(
        "outcome={} via={} elapsed={:.0}ms text={:?} status={:?} navigations={}",
        state.outcome,
        state.via,
        state.elapsed_ms,
        state.text_length,
        state.http_status,
        state.navigations
    );
    if state.blocked() {
        println!(
            "BLOCKED: this is a bot wall, not the page ({})",
            state.reason
        );
    } else if !state.is_settled() {
        println!("did not settle: {}", state.reason);
    }
    watch.close();
    Ok(())
}

/// A CDP connection that keeps the events it meets while waiting for a reply.
///
/// That queue is the part a settle watch depends on: lifecycle events arrive
/// interleaved with command replies, and a transport that drops whatever is not
/// the reply it wanted throws away the fallback's only evidence.
struct Cdp {
    socket: WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    next_id: u64,
    session_id: Option<String>,
    events: VecDeque<(String, Value)>,
}

impl Cdp {
    fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value, Error> {
        self.next_id += 1;
        let id = self.next_id;
        let mut envelope = json!({"id": id, "method": method, "params": params});
        if let Some(session_id) = session.or(self.session_id.as_deref()) {
            envelope["sessionId"] = json!(session_id);
        }
        self.socket
            .send(Message::Text(envelope.to_string().into()))?;
        loop {
            let Message::Text(text) = self.socket.read()? else {
                continue;
            };
            let value: Value = serde_json::from_str(&text)?;
            if value["id"].as_u64() == Some(id) {
                return Ok(value);
            }
            self.queue_event(value);
        }
    }

    fn queue_event(&mut self, value: Value) {
        if let (Some(method), Some(params)) = (value["method"].as_str(), value.get("params")) {
            self.events.push_back((method.to_string(), params.clone()));
        }
    }
}

impl BlockingPageSession for Cdp {
    type Error = Error;

    fn send(&mut self, method: &'static str, params: Value) -> Result<Value, Self::Error> {
        // The whole envelope, deliberately: `Chromeleon.waitForSettle` on a
        // browser launched without --page-settle answers with an `error`
        // member, and the watch reads -32601 straight out of it.
        self.call(method, params, None)
    }

    fn next_event(&mut self, timeout: Duration) -> Option<(String, Value)> {
        if let Some(event) = self.events.pop_front() {
            return Some(event);
        }
        set_read_timeout(&mut self.socket, timeout).ok()?;
        let read = self.socket.read();
        let _ = set_read_timeout(&mut self.socket, Duration::from_secs(30));
        let Ok(Message::Text(text)) = read else {
            return None; // timed out, which is what the watch asked about
        };
        let value: Value = serde_json::from_str(&text).ok()?;
        self.queue_event(value);
        self.events.pop_front()
    }
}

/// The settle watch turns `next_event`'s timeout into a socket read timeout;
/// without one a blocking read waits for a message that may never come.
fn set_read_timeout(
    socket: &mut WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    timeout: Duration,
) -> Result<(), Error> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(Some(timeout))?,
        // A TLS DevTools endpoint is not a thing this example needs to handle.
        _ => return Err("expected a plain ws:// DevTools endpoint".into()),
    }
    Ok(())
}
