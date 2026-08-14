//! `devtools_url` against a server that behaves the way Chrome's does.
//!
//! Two things about the real `/json/version` endpoint break the obvious
//! implementation, and both are in here as regressions:
//!
//! * it **pretty-prints** the JSON across several lines, so a reader that keeps
//!   only lines starting with `{` keeps the string `"{"` and nothing else;
//! * it **ignores `Connection: close`** and holds the socket open, so reading to
//!   EOF never returns and the launch hangs forever instead of failing.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use chromeleon::launch::devtools_url;

const PRETTY_BODY: &str = "{\n   \"Browser\": \"Chromeleon/151.0.7922.71\",\n   \"Protocol-Version\": \"1.3\",\n   \"webSocketDebuggerUrl\": \"ws://127.0.0.1:9222/devtools/browser/abc-123\"\n}\n";

/// Serve one request, then behave as `keep_open` says.
fn serve(body: &'static str, with_content_length: bool, keep_open: bool) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        read_request(&stream);
        let mut out = &stream;
        let headers = if with_content_length {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
        } else {
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n".to_string()
        };
        let _ = out.write_all(headers.as_bytes());
        let _ = out.write_all(body.as_bytes());
        let _ = out.flush();
        if keep_open {
            // Chrome does exactly this: the response is complete, the socket is
            // not. A reader waiting for EOF waits forever.
            thread::sleep(Duration::from_secs(30));
        }
    });
    port
}

fn read_request(stream: &TcpStream) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        if line == "\r\n" || line == "\n" {
            break;
        }
        line.clear();
    }
}

#[test]
fn a_pretty_printed_body_survives_and_the_open_socket_does_not_hang() {
    let port = serve(PRETTY_BODY, true, true);
    let started = Instant::now();
    let url = devtools_url(port, Duration::from_secs(10)).expect("should read the URL");
    assert_eq!(url, "ws://127.0.0.1:9222/devtools/browser/abc-123");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?} — it waited for a close that never comes",
        started.elapsed()
    );
}

#[test]
fn a_response_without_content_length_still_parses() {
    // Not what Chrome sends today, but the fallback path has to work or a proxy
    // in front of the endpoint changes the meaning of a launch.
    let port = serve(PRETTY_BODY, false, false);
    let url = devtools_url(port, Duration::from_secs(10)).expect("should read the URL");
    assert_eq!(url, "ws://127.0.0.1:9222/devtools/browser/abc-123");
}

#[test]
fn a_silent_port_times_out_rather_than_hanging() {
    // Bound but never answering: the browser is up, the endpoint is not.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let _keep = listener.accept();
        thread::sleep(Duration::from_secs(30));
    });
    let started = Instant::now();
    let err = devtools_url(port, Duration::from_secs(2)).expect_err("should time out");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(20), "hung: {err}");
}

#[test]
fn nothing_listening_is_a_timeout_with_the_reason_attached() {
    // Port 1 on loopback: nothing is there, and nothing may bind it either.
    let err = devtools_url(1, Duration::from_millis(300)).expect_err("should fail");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        err.to_string().contains("127.0.0.1:1"),
        "the message should name the endpoint: {err}"
    );
}
