//! The shared cross-language corpus, run against this client.
//!
//! `../conformance/vectors.json` is the repository's anti-drift artefact: 94
//! proxy inputs with the answer every client must give, measured from the
//! Python and Node clients and, for the server string, from the driver's own
//! normalizer (`playwright-core normalizeProxySettings`) — the function the
//! registered string has to be a fixed point of.
//!
//! A case where the two older clients disagree carries the ruling that decided
//! it and the loser's answer, so this test says exactly which client's
//! behaviour it is reproducing rather than silently picking one.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chromeleon::{Error, ProxySpec};
use serde_json::Value;

fn corpus() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("conformance")
        .join("vectors.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the shared conformance corpus is required: {} ({e})",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("conformance corpus is not valid JSON")
}

/// The corpus's error vocabulary, which is this crate's error enum by design.
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

fn run_case(input: &Value) -> Result<ProxySpec, Error> {
    match input["form"].as_str().expect("case input needs a form") {
        "url" => ProxySpec::parse(input["value"].as_str().expect("url input is a string")),
        "parts" => {
            let parts = &input["value"];
            let server = parts["server"]
                .as_str()
                .expect("representable cases have a string server");
            ProxySpec::new(
                server,
                parts["username"].as_str(),
                parts["password"].as_str(),
            )
        }
        other => panic!("unknown input form {other:?}"),
    }
}

#[test]
fn every_corpus_case_matches() {
    let corpus = corpus();
    assert_eq!(
        corpus["spec_version"].as_u64(),
        Some(chromeleon::SPEC_VERSION as u64),
        "the corpus is for a different handshake spec version than this client implements"
    );
    let cases = corpus["cases"].as_array().expect("corpus has cases");
    assert!(cases.len() >= 90, "corpus shrank: {} cases", cases.len());

    let mut failures: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut basis_counts: BTreeMap<String, usize> = BTreeMap::new();

    for case in cases {
        let id = case["id"].as_str().unwrap_or("?");
        if let Some(reason) = case
            .pointer("/unrepresentable/rust")
            .and_then(Value::as_str)
        {
            skipped.push(format!("{id}: {reason}"));
            continue;
        }
        *basis_counts
            .entry(case["basis"].as_str().unwrap_or("?").to_string())
            .or_default() += 1;

        let expect = &case["expect"];
        let got = run_case(&case["input"]);
        let mismatch = match (&got, expect.get("error").and_then(Value::as_str)) {
            (Err(e), Some(kind)) => {
                (kind_of(e) != kind).then(|| format!("error {} (want {kind})", kind_of(e)))
            }
            (Err(e), None) => Some(format!("error {e}")),
            (Ok(spec), Some(kind)) => Some(format!(
                "no error (want {kind}); got server {}",
                spec.server()
            )),
            (Ok(spec), None) => {
                let want_server = expect["server"].as_str().unwrap_or_default();
                let want_user = expect["username"].as_str();
                let want_pass = expect["password"].as_str();
                let want_auth = expect["authenticated"].as_bool().unwrap_or(false);
                if spec.server() != want_server
                    || spec.username() != want_user
                    || spec.password() != want_pass
                    || spec.authenticated() != want_auth
                {
                    Some(format!(
                        "server {:?} user {:?} pass {:?} auth {} (want {:?} / {:?} / {:?} / {})",
                        spec.server(),
                        spec.username(),
                        spec.password(),
                        spec.authenticated(),
                        want_server,
                        want_user,
                        want_pass,
                        want_auth
                    ))
                } else {
                    None
                }
            }
        };
        if let Some(detail) = mismatch {
            failures.push(format!(
                "{id} [{}] {}: {detail}",
                case["basis"].as_str().unwrap_or("?"),
                case["note"].as_str().unwrap_or("")
            ));
        }
    }

    // A silent cap reads as coverage. Say what was not run and on whose
    // authority each case passed.
    eprintln!(
        "conformance: {} cases by basis {basis_counts:?}",
        cases.len()
    );
    for skip in &skipped {
        eprintln!("conformance: SKIPPED {skip}");
    }
    assert!(
        failures.is_empty(),
        "{} conformance failures:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
    assert_eq!(skipped.len(), 3, "the set of unrepresentable cases changed");
}

/// Invariant 1 in its strongest form: normalizing is a fixed point.
///
/// The driver re-normalizes the server it sends to `createBrowserContext`, so a
/// registered string that is not already normalized will not match it. If
/// `normalize(normalize(x)) != normalize(x)` for any input, some proxy in the
/// corpus registers one string and creates a context with another.
#[test]
fn normalization_is_idempotent_across_the_corpus() {
    let corpus = corpus();
    let mut checked = 0;
    for case in corpus["cases"].as_array().unwrap() {
        if let Some(server) = case["expect"]["server"].as_str() {
            let renormalized = chromeleon::normalize_server(server).unwrap_or_else(|e| {
                panic!("{}: re-normalizing {server:?} failed: {e}", case["id"])
            });
            assert_eq!(
                renormalized, server,
                "{}: normalization is not a fixed point",
                case["id"]
            );
            checked += 1;
        }
    }
    assert!(checked >= 70, "only {checked} servers checked");
}

/// The oracle column: where the corpus recorded what the driver itself makes of
/// the RAW input, our normalization must agree with it — that is what
/// byte-identity means in practice.
#[test]
fn we_agree_with_the_driver_oracle() {
    let corpus = corpus();
    let mut compared = 0;
    let mut disagreements = Vec::new();
    for case in corpus["cases"].as_array().unwrap() {
        let (Some(oracle), Some(expected)) = (
            case["playwright_normalizes_input_to"].as_str(),
            case["expect"]["server"].as_str(),
        ) else {
            continue;
        };
        // Two kinds of oracle reading are not comparable. Its catch-branch
        // invents the host `http` for junk input, which is the failure this
        // client refuses to reproduce; and it throws outright on input it will
        // not take at all (`" gw.example.com:3128 "` — untrimmed whitespace
        // reaches its second `new URL` and is rejected). Neither is a server
        // string to agree with. It never sees the raw input from us anyway: we
        // hand it the normalized one, which the idempotence test proves is a
        // fixed point.
        if oracle == "http://http" || !oracle.contains("://") {
            continue;
        }
        compared += 1;
        if oracle != expected {
            disagreements.push(format!(
                "{}: driver says {oracle}, corpus expects {expected}",
                case["id"]
            ));
        }
    }
    assert!(compared >= 60, "only {compared} oracle comparisons");
    assert!(
        disagreements.is_empty(),
        "server strings that would not be consumed:\n  {}",
        disagreements.join("\n  ")
    );
}
