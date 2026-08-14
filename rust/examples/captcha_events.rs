//! Watch the built-in solver work, over a raw DevTools WebSocket.
//!
//! ```sh
//! # Chromeleon must have been launched with --captcha-solver (Launcher::captcha(true))
//! ws=$(curl -s http://127.0.0.1:9222/json/version | grep -o 'ws://[^"]*')
//! cargo run --example captcha_events -- "$ws" https://example.com/with-a-recaptcha
//! ```
//!
//! The solver is automatic: it detects the widget, solves it and writes the
//! token whether or not anyone is listening. The `Chromeleon` CDP domain only
//! reports what it is doing, and its events go to the DevTools session, not to
//! the page. Enabling it is a **page** session command, which is why this
//! attaches to a target first.

use std::time::{Duration, Instant};

use chromeleon::captcha::{CaptchaEvent, ENABLE_METHOD};
use serde_json::{json, Value};
use tungstenite::Message;

type Error = Box<dyn std::error::Error>;

fn main() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let ws_url = args.next().ok_or("usage: <browser-ws-url> <page-url>")?;
    let page_url = args
        .next()
        .unwrap_or_else(|| "https://example.com".to_string());

    let (mut socket, _) = tungstenite::connect(&ws_url)?;
    let mut id = 0u64;
    let mut call = |socket: &mut tungstenite::WebSocket<_>,
                    method: &str,
                    params: Value,
                    session: Option<&str>|
     -> Result<Value, Error> {
        id += 1;
        let mut envelope = json!({"id": id, "method": method, "params": params});
        if let Some(session_id) = session {
            envelope["sessionId"] = json!(session_id);
        }
        socket.send(Message::Text(envelope.to_string().into()))?;
        loop {
            if let Message::Text(text) = socket.read()? {
                let value: Value = serde_json::from_str(&text)?;
                if value["id"].as_u64() == Some(id) {
                    return Ok(value);
                }
            }
        }
    };

    let target = call(
        &mut socket,
        "Target.createTarget",
        json!({ "url": page_url }),
        None,
    )?;
    let target_id = target["result"]["targetId"]
        .as_str()
        .ok_or("no targetId")?
        .to_string();

    // Flat mode: replies and events carry a sessionId instead of being wrapped.
    let attached = call(
        &mut socket,
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        None,
    )?;
    let session_id = attached["result"]["sessionId"]
        .as_str()
        .ok_or("no sessionId")?
        .to_string();

    call(&mut socket, ENABLE_METHOD, json!({}), Some(&session_id))?;
    println!("watching {page_url} for challenges (60s)");

    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let Message::Text(text) = socket.read()? else {
            continue;
        };
        let value: Value = serde_json::from_str(&text)?;
        let (Some(method), Some(params)) = (value["method"].as_str(), value.get("params")) else {
            continue;
        };
        let Some(event) = CaptchaEvent::parse(method, params) else {
            continue;
        };
        match &event {
            CaptchaEvent::Detected { sitekey } => println!("detected  {sitekey}"),
            CaptchaEvent::Solving { sitekey, method } => {
                println!("solving   {sitekey} via {method:?}")
            }
            CaptchaEvent::Solved {
                attempts, time_ms, ..
            } => println!("solved    in {time_ms:.0}ms after {attempts} attempt(s)"),
            CaptchaEvent::Failed {
                attempts, reason, ..
            } => println!("failed    after {attempts} attempt(s): {reason}"),
            CaptchaEvent::SolverEvalResult { result } => println!("eval      {result}"),
            // The enum is #[non_exhaustive]: a newer binary can report a
            // lifecycle step this build predates, and that must not stop the run.
            other => println!("event     {other:?}"),
        }
        if event.is_terminal() {
            break;
        }
    }
    Ok(())
}
