//! What the pool has to guarantee, measured rather than asserted.
//!
//! A pool exists for throughput, so "one slow endpoint does not hold up the
//! others" is a correctness property here, not a nicety. It was not true of the
//! first implementation: a single pool-wide lock was held across the caller's
//! `connect`, and an already-warm endpoint's acquire waited out an unrelated
//! endpoint's connect.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chromeleon::perf::BrowserPool;

#[derive(Debug, PartialEq, Eq)]
struct Conn(String);

async fn slow_connect(endpoint: String, delay: Duration) -> Result<Conn, std::io::Error> {
    tokio::time::sleep(delay).await;
    Ok(Conn(endpoint))
}

#[tokio::test]
async fn a_slow_connect_on_one_endpoint_does_not_hold_up_another() {
    let pool = Arc::new(BrowserPool::<Conn>::new(["ep-a", "ep-b"]).unwrap());

    // Warm ep-a (cursor 0), then leave the cursor pointing back at it.
    pool.acquire(|e| slow_connect(e, Duration::ZERO))
        .await
        .unwrap();
    assert_eq!(pool.warm_count(), 1);

    // Start a slow connect on ep-b (cursor 1).
    let slow_pool = Arc::clone(&pool);
    let slow = tokio::spawn(async move {
        slow_pool
            .acquire(|e| slow_connect(e, Duration::from_millis(600)))
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(80)).await;

    // …and take the warm ep-a (cursor 2 → ep-a) while it is in flight.
    let started = Instant::now();
    let warm = pool
        .acquire(|e| slow_connect(e, Duration::from_secs(30)))
        .await
        .unwrap();
    let waited = started.elapsed();

    assert_eq!(warm.0, "ep-a", "cursor landed somewhere unexpected");
    assert!(
        waited < Duration::from_millis(200),
        "a warm endpoint waited {waited:?} behind another endpoint's connect"
    );
    assert_eq!(slow.await.unwrap().0, "ep-b");
}

#[tokio::test]
async fn racing_tasks_on_one_cold_endpoint_make_exactly_one_connection() {
    let pool = Arc::new(BrowserPool::<Conn>::new(["only"]).unwrap());
    let connects = Arc::new(AtomicUsize::new(0));

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let pool = Arc::clone(&pool);
        let connects = Arc::clone(&connects);
        tasks.push(tokio::spawn(async move {
            pool.acquire(|e| {
                let connects = Arc::clone(&connects);
                async move {
                    connects.fetch_add(1, Ordering::SeqCst);
                    slow_connect(e, Duration::from_millis(50)).await
                }
            })
            .await
            .unwrap()
        }));
    }
    let mut handed_out = Vec::new();
    for task in tasks {
        handed_out.push(task.await.unwrap());
    }

    assert_eq!(connects.load(Ordering::SeqCst), 1, "single-flight broke");
    for connection in &handed_out {
        assert!(
            Arc::ptr_eq(connection, &handed_out[0]),
            "the racers got different connections"
        );
    }
    assert_eq!(pool.warm_count(), 1);
}

#[tokio::test]
async fn evict_and_drain_are_not_blocked_by_an_unrelated_connect() {
    let pool = Arc::new(BrowserPool::<Conn>::new(["ep-a", "ep-b"]).unwrap());
    pool.acquire(|e| slow_connect(e, Duration::ZERO))
        .await
        .unwrap(); // warm ep-a

    let slow_pool = Arc::clone(&pool);
    let slow = tokio::spawn(async move {
        slow_pool
            .acquire(|e| slow_connect(e, Duration::from_millis(600)))
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(80)).await;

    // These are the operator's recovery and shutdown paths; a connect on some
    // other endpoint must not be able to wedge them.
    let evicted = tokio::time::timeout(Duration::from_millis(150), pool.evict("ep-a"))
        .await
        .expect("evict blocked behind an unrelated connect");
    assert_eq!(evicted.map(|c| c.0.clone()), Some("ep-a".to_string()));

    let drained = tokio::time::timeout(Duration::from_millis(150), pool.drain())
        .await
        .expect("drain blocked behind an unrelated connect");
    // ep-a was just evicted and ep-b is still connecting, so there is nothing
    // to hand back — the point is that it RETURNED.
    assert!(drained.is_empty(), "unexpected: {drained:?}");

    assert_eq!(slow.await.unwrap().0, "ep-b");
}

#[tokio::test]
async fn a_cancelled_acquire_leaves_the_pool_usable() {
    let pool = BrowserPool::<Conn>::new(["only"]).unwrap();
    let hung = tokio::time::timeout(
        Duration::from_millis(50),
        pool.acquire(|e| slow_connect(e, Duration::from_secs(30))),
    )
    .await;
    assert!(hung.is_err(), "the connect should not have finished");

    let recovered = tokio::time::timeout(
        Duration::from_millis(500),
        pool.acquire(|e| slow_connect(e, Duration::ZERO)),
    )
    .await
    .expect("the slot was never released")
    .unwrap();
    assert_eq!(recovered.0, "only");
}
