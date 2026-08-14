//! Run the shared corpus through this client and print the results.
//!
//! Emitter contract (see `../../conformance/README.md`): read `vectors.json`
//! from argv[1], write `{client, results: [{id, ok, ...}]}` to stdout. Nothing
//! is judged here — `check.py` does the comparing, so an emitter can never
//! quietly grade itself.
//!
//! `cargo test --test conformance` checks the same corpus in-process; this
//! exists so the cross-language runner can compare all three clients side by
//! side.

use chromeleon::{Error, ProxySpec};
use serde_json::{json, Value};

fn kind_of(error: &Error) -> &'static str {
    match error {
        Error::MissingServer => "missing_server",
        Error::InvalidServer { .. } => "invalid_server",
        Error::UndecodableCredential { .. } => "undecodable_credential",
        Error::Unauthenticated { .. } => "unauthenticated",
        Error::UnsupportedScheme { .. } => "unsupported_scheme",
        Error::RegistrationRefused { .. } => "registration_refused",
        _ => "unknown",
    }
}

fn run(case: &Value) -> Value {
    let id = case["id"].clone();
    if let Some(reason) = case
        .pointer("/unrepresentable/rust")
        .and_then(Value::as_str)
    {
        return json!({"id": id, "skipped": reason});
    }
    let input = &case["input"];
    let parsed = match input["form"].as_str() {
        Some("url") => ProxySpec::parse(input["value"].as_str().unwrap_or_default()),
        Some("parts") => {
            let parts = &input["value"];
            match parts["server"].as_str() {
                Some(server) => ProxySpec::new(
                    server,
                    parts["username"].as_str(),
                    parts["password"].as_str(),
                ),
                None => Err(Error::MissingServer),
            }
        }
        other => panic!("unknown input form {other:?}"),
    };
    match parsed {
        Ok(spec) => json!({
            "id": id,
            "ok": true,
            "server": spec.server(),
            "username": spec.username(),
            "password": spec.password(),
            "authenticated": spec.authenticated(),
        }),
        Err(e) => json!({"id": id, "ok": false, "kind": kind_of(&e), "error": e.to_string()}),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: conformance_emit <vectors.json>")?;
    let corpus: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let results: Vec<Value> = corpus["cases"]
        .as_array()
        .ok_or("corpus has no cases")?
        .iter()
        .map(run)
        .collect();
    println!("{}", json!({"client": "rust", "results": results}));
    Ok(())
}
