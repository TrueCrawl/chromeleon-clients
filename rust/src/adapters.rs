//! Driver adapters over the transport-agnostic core.
//!
//! Rust has no single browser driver, so the adapter here is the general one:
//! you supply how to *send* a browser-level CDP command and how to *create* a
//! context, and the handshake runs between them under the registration lock.
//! That is a few lines for chromiumoxide, for `headless_chrome`, or for a raw
//! DevTools WebSocket, and it means this crate never has to own your browser or
//! pin your driver's version.
//!
//! Whatever your driver returns comes back out unchanged.

use std::future::Future;

use serde_json::Value;

use crate::core::{
    check_registration, Error, IntoProxySpec, Result, CREATE_CONTEXT_METHOD, CREDENTIALS_METHOD,
};
use crate::registration::{with_proxy_registration, with_proxy_registration_blocking, ConnKey};

/// Run the two-command handshake with a driver that owns context creation.
///
/// `send` issues a **browser-level** CDP command (not a page session);
/// `create_context` makes the context with the server string it is given, which
/// must be exactly the string passed in — that is what consumes the
/// registration.
///
/// ```no_run
/// # use chromeleon::adapters::new_proxy_context_with;
/// # use chromeleon::Error;
/// # struct MyConn;
/// # impl MyConn {
/// #   async fn send(&self, _m: &str, _p: serde_json::Value) -> Result<serde_json::Value, Error> { Ok(serde_json::json!({})) }
/// #   async fn create_context(&self, _s: String) -> Result<String, Error> { Ok(String::new()) }
/// # }
/// # async fn demo(conn: &MyConn) -> Result<String, Error> {
/// let context_id = new_proxy_context_with(
///     "ws://127.0.0.1:9222/devtools/browser/abc",
///     "http://user:pass@gateway:12321",
///     |method, params| conn.send(method, params),
///     |server| conn.create_context(server),
/// ).await?;
/// # Ok(context_id) }
/// ```
pub async fn new_proxy_context_with<P, S, SFut, C, CFut, T, E>(
    conn: impl Into<ConnKey>,
    proxy: P,
    send: S,
    create_context: C,
) -> Result<T, E>
where
    P: IntoProxySpec,
    S: FnOnce(&'static str, Value) -> SFut,
    SFut: Future<Output = Result<Value, E>>,
    C: FnOnce(String) -> CFut,
    CFut: Future<Output = Result<T, E>>,
    E: From<Error>,
{
    with_proxy_registration(conn, proxy, |spec| async move {
        let result = send(CREDENTIALS_METHOD, spec.credentials_params().to_value()).await?;
        check_registration(&result).map_err(E::from)?;
        // Still holding the registration: this call is what consumes it.
        create_context(spec.server().to_string()).await
    })
    .await
}

/// The same handshake for a connection that only speaks raw CDP.
///
/// Both commands go through `send`, and the `Target.createBrowserContext`
/// result is returned as-is — its `browserContextId` is what you attach pages
/// to.
///
/// `send` should return the command's **`result` object**. Returning the whole
/// CDP envelope also works — [`check_registration`] looks for an `error` member
/// either way — but then the `browserContextId` is one level down, under
/// `result`.
///
/// ```no_run
/// # use chromeleon::adapters::new_proxy_context_raw;
/// # use chromeleon::Error;
/// # struct Ws;
/// # impl Ws { async fn call(&self, _m: &str, _p: serde_json::Value) -> Result<serde_json::Value, Error> { Ok(serde_json::json!({})) } }
/// # async fn demo(ws: &Ws) -> Result<(), Error> {
/// let created = new_proxy_context_raw(
///     "ws://127.0.0.1:9222/devtools/browser/abc",
///     "http://user:pass@gateway:12321",
///     |method, params| ws.call(method, params),
/// ).await?;
/// let context_id = created["browserContextId"].as_str().unwrap_or_default();
/// # let _ = context_id; Ok(()) }
/// ```
pub async fn new_proxy_context_raw<P, S, SFut, E>(
    conn: impl Into<ConnKey>,
    proxy: P,
    send: S,
) -> Result<Value, E>
where
    P: IntoProxySpec,
    S: FnMut(&'static str, Value) -> SFut,
    SFut: Future<Output = Result<Value, E>>,
    E: From<Error>,
{
    let mut send = send;
    with_proxy_registration(conn, proxy, |spec| async move {
        let result = send(CREDENTIALS_METHOD, spec.credentials_params().to_value()).await?;
        check_registration(&result).map_err(E::from)?;
        let created = send(
            CREATE_CONTEXT_METHOD,
            spec.create_context_params().to_value(),
        )
        .await?;
        check_registration(&created).map_err(E::from)?;
        Ok(created)
    })
    .await
}

/// Blocking form of [`new_proxy_context_with`], for synchronous drivers such as
/// `headless_chrome`.
///
/// ⚠️ Blocks the calling thread on the registration lock. Do not call it from
/// inside an async task; see [`Registry::acquire_blocking`](crate::registration::Registry::acquire_blocking).
pub fn new_proxy_context_with_blocking<P, S, C, T, E>(
    conn: impl Into<ConnKey>,
    proxy: P,
    send: S,
    create_context: C,
) -> Result<T, E>
where
    P: IntoProxySpec,
    S: FnOnce(&'static str, Value) -> Result<Value, E>,
    C: FnOnce(&str) -> Result<T, E>,
    E: From<Error>,
{
    with_proxy_registration_blocking(conn, proxy, |spec| {
        let result = send(CREDENTIALS_METHOD, spec.credentials_params().to_value())?;
        check_registration(&result).map_err(E::from)?;
        create_context(spec.server())
    })
}

/// Blocking form of [`new_proxy_context_raw`].
pub fn new_proxy_context_raw_blocking<P, S, E>(
    conn: impl Into<ConnKey>,
    proxy: P,
    send: S,
) -> Result<Value, E>
where
    P: IntoProxySpec,
    S: FnMut(&'static str, Value) -> Result<Value, E>,
    E: From<Error>,
{
    let mut send = send;
    with_proxy_registration_blocking(conn, proxy, |spec| {
        let result = send(CREDENTIALS_METHOD, spec.credentials_params().to_value())?;
        check_registration(&result).map_err(E::from)?;
        let created = send(
            CREATE_CONTEXT_METHOD,
            spec.create_context_params().to_value(),
        )?;
        check_registration(&created).map_err(E::from)?;
        Ok(created)
    })
}
