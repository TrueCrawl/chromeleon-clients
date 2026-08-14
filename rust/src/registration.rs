//! The lock the single-use registration requires.
//!
//! The browser holds a pending registration per `(root CDP connection,
//! proxyServer)`, and the matching `Target.createBrowserContext` consumes it —
//! single-use — consumed even when the create then fails, so a retry must
//! preregister again. The register→create pair therefore has to be
//! **serialized** per `(connection, server)`. Different servers on the same
//! connection may run concurrently, and usually should — that is the throughput
//! case.
//!
//! **Version note, because the browser's side of this changed.** Up to and
//! including v151.4 — the binary currently published as `latest` — a second
//! registration for the same pair is *rejected* outright while one is pending.
//! From v151.5 the browser instead **defers the reply** until the slot frees,
//! which serializes the pair browser-side (bounded: a queue per endpoint and
//! per root connection, and a timeout), and it accepts an optional correlation
//! token, `credentialsId`, paired with `proxyCredentialsId` on the context —
//! tokened registrations are independent and never queue at all. This client
//! always takes the lock: it is required against .4, and against .5 it costs
//! nothing while keeping the waiting local instead of in flight. See
//! [`CredentialsParams::with_credentials_id`](crate::core::CredentialsParams::with_credentials_id)
//! for the token, which is opt-in because an older binary does not know the
//! field.
//!
//! Hold a [`Registration`] across BOTH commands:
//!
//! ```no_run
//! # use chromeleon::{registration::{ConnKey, proxy_registration}, check_registration, CREDENTIALS_METHOD, CREATE_CONTEXT_METHOD};
//! # async fn demo(send: impl Fn(&str, serde_json::Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = serde_json::Value>>>) -> Result<(), chromeleon::Error> {
//! let reg = proxy_registration(ConnKey::new("ws://127.0.0.1:9222/devtools/browser/abc"),
//!                              "http://user:pass@gw:12321").await?;
//! check_registration(&send(CREDENTIALS_METHOD, reg.credentials_params().to_value()).await)?;
//! let _ctx = send(CREATE_CONTEXT_METHOD, reg.create_context_params().to_value()).await;
//! drop(reg);  // released only now: the create is what consumes the registration
//! # Ok(()) }
//! ```
//!
//! Dropping the [`Registration`] early — before the context is created — is the
//! one way to misuse this. The compiler will not stop you, but the browser
//! will: another task can then register over your slot and your context is
//! created against credentials that are no longer pending.
//!
//! The same applies to **cancellation**: if the future holding a registration
//! is dropped between the two commands (a `timeout`, a lost `select!` branch),
//! this client releases its lock, but the browser still holds a pending
//! registration for that endpoint. Closing the CDP connection clears it (the
//! browser answers any waiter with "the connection that owns this registration
//! closed"); an abandoned registration on a connection you keep open is not
//! documented to expire on its own. Until it is consumed or the connection
//! goes, the next attempt against a v151.4 binary is refused as already
//! outstanding. If you cancel handshakes, use a `credentialsId` — tokened
//! registrations do not contend.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, PoisonError};

use async_lock::{Mutex as AsyncMutex, MutexGuardArc};

use crate::core::{
    CreateContextParams, CredentialsParams, Error, IntoProxySpec, ProxySpec, Result,
};

/// Identity of one CDP connection, for locking only.
///
/// The browser keys pending registrations on the **root** session, so this must
/// name the root DevTools connection — its WebSocket URL is the natural
/// spelling, and is what the adapters use. Two consequences worth knowing:
///
/// * Several browser-level CDP sessions opened over one connection, and any
///   child of `Target.attachToBrowserTarget`, all share **one** slot. Keying on
///   a per-call session handle instead would split one slot into several and
///   un-serialize the handshake.
/// * Two separate WebSockets to the same browser are genuinely independent and
///   must get different keys.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnKey(Arc<str>);

impl ConnKey {
    /// Key on a stable string — normally the browser's DevTools WebSocket URL.
    pub fn new(id: impl Into<String>) -> Self {
        ConnKey(Arc::from(id.into().as_str()))
    }

    /// Key on an `Arc`'s allocation address.
    ///
    /// Stable for the life of the `Arc`, because the allocation does not move
    /// when the `Arc` is cloned or the handle is moved. Prefer [`ConnKey::new`]
    /// with the endpoint when you have it: an address can be reused after the
    /// last clone is dropped, and two unrelated connections would then share a
    /// lock — slower, never incorrect.
    pub fn from_arc<T>(conn: &Arc<T>) -> Self {
        ConnKey(Arc::from(format!("arc:{:p}", Arc::as_ptr(conn)).as_str()))
    }

    /// Key on a reference's address.
    ///
    /// ⚠️ Only sound when the referent does not move — a connection held in a
    /// local and later moved has a DIFFERENT address, which silently splits one
    /// connection into two lock keys and un-serializes the handshake. Use
    /// [`ConnKey::new`] or [`ConnKey::from_arc`] unless you are certain.
    pub fn from_ptr<T>(conn: &T) -> Self {
        ConnKey(Arc::from(format!("ptr:{:p}", conn as *const T).as_str()))
    }

    /// The key as it is stored.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ConnKey {
    fn from(s: &str) -> Self {
        ConnKey::new(s)
    }
}

impl From<String> for ConnKey {
    fn from(s: String) -> Self {
        ConnKey::new(s)
    }
}

impl From<&String> for ConnKey {
    fn from(s: &String) -> Self {
        ConnKey::new(s.as_str())
    }
}

impl std::fmt::Display for ConnKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

type Slot = Arc<AsyncMutex<()>>;

#[derive(Default)]
struct Inner {
    slots: StdMutex<HashMap<(ConnKey, String), Slot>>,
}

/// The set of registration slots, one per `(connection, proxyServer)`.
///
/// [`Registry::global`] is the one every adapter uses; construct your own only
/// to isolate tests from each other.
#[derive(Clone, Default)]
pub struct Registry(Arc<Inner>);

impl Registry {
    /// An empty registry, sharing nothing with [`Registry::global`].
    pub fn new() -> Self {
        Registry::default()
    }

    /// The process-wide registry the adapters and free functions use.
    pub fn global() -> &'static Registry {
        static GLOBAL: OnceLock<Registry> = OnceLock::new();
        GLOBAL.get_or_init(Registry::new)
    }

    /// Number of live slots. Test/diagnostic only — slots are dropped as they
    /// drain, so a growing number here means registrations are being leaked.
    pub fn tracked_slots(&self) -> usize {
        self.0
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn slot(&self, key: &(ConnKey, String)) -> Slot {
        let mut slots = self.0.slots.lock().unwrap_or_else(PoisonError::into_inner);
        // No entry() with a borrowed key on stable, and the clone only happens
        // on a miss anyway.
        if let Some(existing) = slots.get(key) {
            return Arc::clone(existing);
        }
        let created: Slot = Arc::new(AsyncMutex::new(()));
        slots.insert(key.clone(), Arc::clone(&created));
        created
    }

    /// Drop a slot nobody else is holding or waiting on.
    ///
    /// Checked under the map lock, which is also what a would-be waiter must
    /// take to clone the slot: if the count is 1 the map is the only owner, so
    /// removing it cannot orphan anyone mid-wait.
    fn prune(&self, key: &(ConnKey, String)) {
        let mut slots = self.0.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if slots
            .get(key)
            .is_some_and(|slot| Arc::strong_count(slot) == 1)
        {
            slots.remove(key);
        }
    }

    fn prepare(
        &self,
        conn: ConnKey,
        proxy: impl IntoProxySpec,
    ) -> Result<(ProxySpec, (ConnKey, String), Slot)> {
        let spec = proxy.into_proxy_spec()?;
        spec.check_registrable()?;
        let key = (conn, spec.server().to_string());
        let slot = self.slot(&key);
        Ok((spec, key, slot))
    }

    /// Take the registration slot for `proxy` on `conn`, waiting if another
    /// handshake for the same pair is in flight.
    pub async fn acquire(
        &self,
        conn: impl Into<ConnKey>,
        proxy: impl IntoProxySpec,
    ) -> Result<Registration> {
        let (spec, key, slot) = self.prepare(conn.into(), proxy)?;
        let guard = slot.lock_arc().await;
        Ok(Registration {
            spec,
            key,
            registry: self.clone(),
            guard: Some(guard),
        })
    }

    /// Blocking form of [`Registry::acquire`], for synchronous drivers.
    ///
    /// ⚠️ Blocks the calling OS thread. Never call it from inside an async task
    /// — it takes the same lock the async path does, so an executor thread
    /// parked here can be waiting on a future that will never be polled.
    pub fn acquire_blocking(
        &self,
        conn: impl Into<ConnKey>,
        proxy: impl IntoProxySpec,
    ) -> Result<Registration> {
        let (spec, key, slot) = self.prepare(conn.into(), proxy)?;
        let guard = slot.lock_arc_blocking();
        Ok(Registration {
            spec,
            key,
            registry: self.clone(),
            guard: Some(guard),
        })
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("tracked_slots", &self.tracked_slots())
            .finish()
    }
}

/// A held registration slot: the credentials may be registered and the context
/// created while this is alive, and not after it is dropped.
#[derive(Debug)]
#[must_use = "the slot is released as soon as this is dropped — hold it across \
              BOTH commands, or another task can register over you"]
pub struct Registration {
    spec: ProxySpec,
    key: (ConnKey, String),
    registry: Registry,
    guard: Option<MutexGuardArc<()>>,
}

impl Registration {
    /// The validated, normalized proxy.
    pub fn spec(&self) -> &ProxySpec {
        &self.spec
    }

    /// The normalized server string — the same bytes in both commands.
    pub fn server(&self) -> &str {
        self.spec.server()
    }

    /// Params for `Target.setProxyCredentials`.
    pub fn credentials_params(&self) -> CredentialsParams<'_> {
        self.spec.credentials_params()
    }

    /// Params for `Target.createBrowserContext`.
    pub fn create_context_params(&self) -> CreateContextParams<'_> {
        self.spec.create_context_params()
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // Release the slot BEFORE pruning, so the strong-count check sees the
        // true number of owners.
        self.guard.take();
        self.registry.prune(&self.key);
    }
}

/// Take the registration slot for `proxy` on `conn` from the global registry.
pub async fn proxy_registration(
    conn: impl Into<ConnKey>,
    proxy: impl IntoProxySpec,
) -> Result<Registration> {
    Registry::global().acquire(conn, proxy).await
}

/// Blocking form of [`proxy_registration`], for synchronous drivers.
pub fn proxy_registration_blocking(
    conn: impl Into<ConnKey>,
    proxy: impl IntoProxySpec,
) -> Result<Registration> {
    Registry::global().acquire_blocking(conn, proxy)
}

/// Run `body` holding the registration slot, and release it after.
///
/// The closure shape of the Node client (`withProxyRegistration`), for callers
/// who would rather not reason about where the guard is dropped. Your error
/// type only has to absorb ours.
///
/// ```
/// # use chromeleon::{registration::with_proxy_registration, Error};
/// # async fn demo() -> Result<(), Error> {
/// let context_id: String = with_proxy_registration("ws://127.0.0.1:9222/x", "http://u:p@gw:1", |spec| async move {
///     // register, then create, both with spec.server()
///     Ok::<_, Error>(spec.server().to_string())
/// }).await?;
/// # let _ = context_id; Ok(()) }
/// ```
pub async fn with_proxy_registration<P, F, Fut, T, E>(
    conn: impl Into<ConnKey>,
    proxy: P,
    body: F,
) -> Result<T, E>
where
    P: IntoProxySpec,
    F: FnOnce(ProxySpec) -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: From<Error>,
{
    let registration = proxy_registration(conn, proxy).await.map_err(E::from)?;
    let out = body(registration.spec().clone()).await;
    drop(registration);
    out
}

/// Blocking form of [`with_proxy_registration`].
///
/// ⚠️ See [`Registry::acquire_blocking`]: not for use inside an async task.
pub fn with_proxy_registration_blocking<P, F, T, E>(
    conn: impl Into<ConnKey>,
    proxy: P,
    body: F,
) -> Result<T, E>
where
    P: IntoProxySpec,
    F: FnOnce(&ProxySpec) -> Result<T, E>,
    E: From<Error>,
{
    let registration = proxy_registration_blocking(conn, proxy).map_err(E::from)?;
    let out = body(registration.spec());
    drop(registration);
    out
}
