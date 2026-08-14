//! What the registration lock has to guarantee.
//!
//! The browser holds one pending registration per `(connection, proxyServer)`
//! and rejects a second until the first is consumed, so these are correctness
//! tests, not performance ones: an interleaved register/create pair is a
//! context that browses without its proxy.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use chromeleon::registration::{with_proxy_registration, ConnKey, Registry};
use chromeleon::{Error, ProxySpec};

use futures_lite::future::{block_on, yield_now, zip};

const PROXY_A: &str = "http://user:pass@gw-a.example.com:12321";
const PROXY_B: &str = "http://user:pass@gw-b.example.com:12321";

/// Log a handshake as two steps with suspension points between them, the way a
/// real one has: `send` and `create_context` are both awaits.
async fn handshake(
    registry: &Registry,
    conn: &str,
    proxy: &str,
    tag: &str,
    log: &Rc<RefCell<Vec<String>>>,
) {
    let reg = registry.acquire(conn, proxy).await.expect("acquire");
    log.borrow_mut().push(format!("{tag}:register"));
    for _ in 0..8 {
        yield_now().await;
    }
    log.borrow_mut().push(format!("{tag}:create"));
    drop(reg);
}

#[test]
fn the_same_connection_and_server_never_interleave() {
    let registry = Registry::new();
    let log = Rc::new(RefCell::new(Vec::new()));
    block_on(zip(
        handshake(&registry, "conn-1", PROXY_A, "A", &log),
        handshake(&registry, "conn-1", PROXY_A, "B", &log),
    ));
    let log = log.borrow().clone();
    // Either order is fine; a pair split by the other one's step is not.
    assert!(
        log == ["A:register", "A:create", "B:register", "B:create"]
            || log == ["B:register", "B:create", "A:register", "A:create"],
        "interleaved: {log:?}"
    );
}

#[test]
fn different_servers_on_one_connection_run_concurrently() {
    let registry = Registry::new();
    let log = Rc::new(RefCell::new(Vec::new()));
    block_on(zip(
        handshake(&registry, "conn-1", PROXY_A, "A", &log),
        handshake(&registry, "conn-1", PROXY_B, "B", &log),
    ));
    let log = log.borrow().clone();
    // Throughput is the point: B must not have waited for A.
    assert_eq!(
        log,
        ["A:register", "B:register", "A:create", "B:create"],
        "different servers were serialized: {log:?}"
    );
}

#[test]
fn different_connections_run_concurrently() {
    let registry = Registry::new();
    let log = Rc::new(RefCell::new(Vec::new()));
    block_on(zip(
        handshake(&registry, "conn-1", PROXY_A, "A", &log),
        handshake(&registry, "conn-2", PROXY_A, "B", &log),
    ));
    assert_eq!(
        log.borrow().clone(),
        ["A:register", "B:register", "A:create", "B:create"]
    );
}

#[test]
fn the_key_is_the_normalized_server_not_the_spelling() {
    let registry = Registry::new();
    let log = Rc::new(RefCell::new(Vec::new()));
    block_on(zip(
        handshake(
            &registry,
            "conn-1",
            "http://u:p@GW-A.example.com:12321",
            "A",
            &log,
        ),
        // Same server after normalization — uppercase host, default port form.
        handshake(&registry, "conn-1", "u:p@gw-a.example.com:12321", "B", &log),
    ));
    let log = log.borrow().clone();
    assert!(
        log == ["A:register", "A:create", "B:register", "B:create"]
            || log == ["B:register", "B:create", "A:register", "A:create"],
        "two spellings of one server did not share a slot: {log:?}"
    );
}

#[test]
fn a_failed_body_still_releases_the_slot() {
    // Deliberately the GLOBAL registry, which is what the free functions and
    // adapters use; the connection key is unique to this test so parallel test
    // threads cannot contend on it.
    const CONN: &str = "conn-failed-body";
    block_on(async {
        let failed: Result<(), Error> = with_proxy_registration(CONN, PROXY_A, |_spec| async {
            Err(Error::RegistrationRefused {
                detail: "browser said no".into(),
            })
        })
        .await;
        assert!(failed.is_err());

        // The next acquirer must not hang.
        let recovered: Result<&str, Error> =
            with_proxy_registration(CONN, PROXY_A, |_spec| async { Ok("recovered") }).await;
        assert_eq!(recovered.unwrap(), "recovered");
    });
}

#[test]
fn slots_are_dropped_when_they_drain() {
    let registry = Registry::new();
    assert_eq!(registry.tracked_slots(), 0);
    block_on(async {
        let a = registry.acquire("conn-1", PROXY_A).await.unwrap();
        let b = registry.acquire("conn-1", PROXY_B).await.unwrap();
        assert_eq!(registry.tracked_slots(), 2);
        drop(a);
        assert_eq!(registry.tracked_slots(), 1);
        drop(b);
    });
    // A long-lived process opens many contexts; leaked slots would be a leak of
    // one mutex per (connection, server) for the life of the program.
    assert_eq!(registry.tracked_slots(), 0);
}

#[test]
fn a_waiting_acquirer_keeps_its_slot_alive() {
    let registry = Registry::new();
    block_on(async {
        let held = registry.acquire("conn-1", PROXY_A).await.unwrap();
        let queued = registry.acquire("conn-1", PROXY_A);
        futures_lite::pin!(queued);
        // Poll the queued acquisition once so it is genuinely waiting, then let
        // the holder go: the slot it is waiting on must not have been pruned
        // out from under it.
        assert!(futures_lite::future::poll_once(queued.as_mut())
            .await
            .is_none());
        assert_eq!(registry.tracked_slots(), 1);
        drop(held);
        let taken = queued.await.unwrap();
        assert_eq!(taken.server(), "http://gw-a.example.com:12321");
    });
    assert_eq!(registry.tracked_slots(), 0);
}

#[test]
fn the_blocking_path_takes_the_same_lock_as_the_async_one() {
    let registry = Arc::new(Registry::new());
    let held = block_on(registry.acquire("conn-1", PROXY_A)).unwrap();

    let acquired = Arc::new(AtomicBool::new(false));
    let thread_registry = Arc::clone(&registry);
    let thread_acquired = Arc::clone(&acquired);
    let waiter = thread::spawn(move || {
        let reg = thread_registry
            .acquire_blocking("conn-1", PROXY_A)
            .expect("blocking acquire");
        thread_acquired.store(true, Ordering::SeqCst);
        drop(reg);
    });

    thread::sleep(Duration::from_millis(100));
    assert!(
        !acquired.load(Ordering::SeqCst),
        "a synchronous driver walked straight into a registration held by an async one"
    );
    drop(held);
    waiter.join().expect("waiter thread");
    assert!(acquired.load(Ordering::SeqCst));
    assert_eq!(registry.tracked_slots(), 0);
}

#[test]
fn preregistration_refuses_what_it_cannot_register() {
    let registry = Registry::new();
    block_on(async {
        // No credentials: nothing to register, and the driver takes this proxy
        // directly.
        assert!(matches!(
            registry
                .acquire("conn-1", "http://gw.example.com:12321")
                .await,
            Err(Error::Unauthenticated { .. })
        ));
        // Username but no password is NOT authenticated.
        assert!(matches!(
            registry
                .acquire("conn-1", "http://user@gw.example.com:12321")
                .await,
            Err(Error::Unauthenticated { .. })
        ));
        // SOCKS cannot carry this handshake.
        assert!(matches!(
            registry
                .acquire("conn-1", "socks5://user:pass@gw:1080")
                .await,
            Err(Error::UnsupportedScheme { .. })
        ));
        // An unparseable server never reaches the lock.
        assert!(matches!(
            registry.acquire("conn-1", "http://gw:notaport").await,
            Err(Error::InvalidServer { .. })
        ));
        assert_eq!(
            registry.tracked_slots(),
            0,
            "a refused proxy left a slot behind"
        );
    });
}

#[test]
fn an_explicit_empty_password_is_registrable() {
    let registry = Registry::new();
    block_on(async {
        let reg = registry
            .acquire("conn-1", "http://user:@gw.example.com:12321")
            .await
            .expect("empty password is a real credential");
        let params = reg.credentials_params();
        assert_eq!(params.username, "user");
        assert_eq!(params.password, "");
        assert_eq!(params.proxy_server, "http://gw.example.com:12321");
    });
}

#[test]
fn both_commands_get_the_same_bytes() {
    let spec = ProxySpec::parse("HTTP://User:Pass@GW.Example.COM:80").unwrap();
    assert_eq!(
        spec.credentials_params().proxy_server,
        spec.create_context_params().proxy_server
    );
    assert_eq!(spec.server(), "http://gw.example.com");
}

#[test]
fn conn_keys_distinguish_connections_but_not_clones() {
    let a = Arc::new("connection");
    let b = Arc::clone(&a);
    assert_eq!(ConnKey::from_arc(&a), ConnKey::from_arc(&b));
    assert_ne!(
        ConnKey::from_arc(&a),
        ConnKey::from_arc(&Arc::new("connection"))
    );
    assert_eq!(ConnKey::new("ws://x"), ConnKey::from("ws://x"));
}
