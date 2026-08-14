//! Launch-latency helpers: pre-supply proxy geo, and pool warm connections.
//!
//! Two independent, opt-in optimizations for the Chromeleon launch tax. Both are
//! transport-only: they change *when* work happens, never the persona the
//! browser presents.
//!
//! * [`sticky_geo_env`] resolves a **sticky** proxy's exit IP and IANA timezone
//!   once and hands them to the browser through `CHROMELEON_EXIT_IP` /
//!   `CHROMELEON_TARGET_TZ`, so the binary skips its own through-proxy geo round
//!   trip on every launch. Only valid when the exit is stable for the session:
//!   pinning a rotating proxy's IP presents a timezone that no longer matches
//!   the egress, which is a fingerprint tell rather than an optimization, so the
//!   helper refuses unless you say [`ProxyRotation::Sticky`].
//! * [`BrowserPool`] keeps one warm connection per already-running CDP endpoint
//!   and hands them out round-robin, so a throughput workload pays the launch
//!   tax zero times per task.
//!
//! Neither takes an HTTP client dependency: you supply the resolver and the
//! connector, so the crate stays driver-agnostic and adds nothing to your build.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Mutex as StdMutex, PoisonError};
use std::time::{Duration, Instant};

use crate::core::ProxySpec;

/// Environment variable the launcher reads for the proxy's exit IP.
pub const EXIT_IP_ENV: &str = "CHROMELEON_EXIT_IP";
/// Environment variable the launcher reads for the target IANA timezone.
pub const TARGET_TZ_ENV: &str = "CHROMELEON_TARGET_TZ";

/// Default lifetime of a cached geo resolution (10 minutes), matching the
/// Python client. A sticky session that rotates on a fixed interval should use
/// something below its rotation period.
pub const DEFAULT_GEO_TTL: Duration = Duration::from_secs(600);

/// Whether the proxy's exit is stable for the session.
///
/// There is no default and no bool: pinning geo for a rotating proxy is a
/// detection tell, and the caller is the only one who knows which they bought.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyRotation {
    /// The exit IP is pinned for the session — geo may be pre-supplied.
    Sticky,
    /// The exit rotates — the binary must resolve geo itself, every launch.
    Rotating,
}

/// A resolved proxy exit.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GeoResolution {
    /// The exit IP the proxy egresses from.
    pub exit_ip: String,
    /// The IANA zone for that IP, when it passed [`is_valid_iana_tz`].
    pub timezone: Option<String>,
}

impl GeoResolution {
    /// A resolution from an exit IP and an optional IANA zone.
    pub fn new(exit_ip: impl Into<String>, timezone: Option<String>) -> Self {
        GeoResolution {
            exit_ip: exit_ip.into(),
            timezone,
        }
    }

    /// The environment variables this resolution contributes.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![(EXIT_IP_ENV.to_string(), self.exit_ip.clone())];
        if let Some(tz) = &self.timezone {
            env.push((TARGET_TZ_ENV.to_string(), tz.clone()));
        }
        env
    }
}

/// The shape gate the binary applies to a timezone: `Area/Location`, no
/// whitespace or control characters, bounded length.
///
/// A malformed field that reaches the launcher becomes the browser's timezone,
/// so a resolution that fails this keeps its IP pin and drops the zone rather
/// than passing it on.
pub fn is_valid_iana_tz(tz: &str) -> bool {
    !tz.is_empty()
        && tz.contains('/')
        && tz.len() < 64
        && !tz.chars().any(|c| c.is_whitespace() || (c as u32) < 0x20)
}

struct CacheEntry {
    resolution: GeoResolution,
    resolved_at: Instant,
}

/// TTL cache of proxy exit resolutions.
///
/// Keyed by `(server, username)`, not by server alone: with a sticky gateway the
/// **username** is what selects the exit (`user-session-a4f2`, `user-country-de`
/// and so on), so two credentials pointing at one `host:port` are two different
/// exits. Keying on the server would pin one of them and hand the other a
/// timezone that does not match its egress — precisely the tell this helper
/// exists to avoid. (The Python client keys on the server; that is a bug.)
#[derive(Default)]
pub struct GeoCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
}

impl GeoCache {
    /// An empty cache, sharing nothing with [`GeoCache::global`].
    pub fn new() -> Self {
        GeoCache::default()
    }

    /// The process-wide cache [`sticky_geo_env`] uses.
    pub fn global() -> &'static GeoCache {
        static GLOBAL: std::sync::OnceLock<GeoCache> = std::sync::OnceLock::new();
        GLOBAL.get_or_init(GeoCache::new)
    }

    /// The exit a proxy actually routes to is chosen by server AND username.
    fn key(proxy: &ProxySpec) -> String {
        format!("{}\0{}", proxy.server(), proxy.username().unwrap_or(""))
    }

    fn get(&self, key: &str, ttl: Duration) -> Option<GeoResolution> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .get(key)
            .filter(|e| e.resolved_at.elapsed() < ttl)
            .map(|e| e.resolution.clone())
    }

    fn put(&self, key: String, resolution: GeoResolution) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.insert(
            key,
            CacheEntry {
                resolution,
                resolved_at: Instant::now(),
            },
        );
    }

    /// Drop every cached resolution — call it on a known rotation.
    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Number of cached entries, fresh or stale.
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// True when nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Resolve through `resolve`, or serve a fresh cached answer.
    ///
    /// A resolver error is never fatal: it returns `None` and the binary
    /// resolves geo itself, exactly as if the helper were not used. Failing the
    /// launch over an optimization would be the worse trade.
    pub fn resolve_with<F, E>(
        &self,
        proxy: &ProxySpec,
        rotation: ProxyRotation,
        ttl: Duration,
        resolve: F,
    ) -> Option<GeoResolution>
    where
        F: FnOnce(&ProxySpec) -> std::result::Result<GeoResolution, E>,
    {
        if rotation != ProxyRotation::Sticky {
            return None;
        }
        let key = Self::key(proxy);
        if let Some(hit) = self.get(&key, ttl) {
            return Some(hit);
        }
        let resolved = sanitize(resolve(proxy).ok()?)?;
        self.put(key, resolved.clone());
        Some(resolved)
    }

    /// [`GeoCache::resolve_with`] with an async resolver.
    pub async fn resolve_with_async<F, Fut, E>(
        &self,
        proxy: &ProxySpec,
        rotation: ProxyRotation,
        ttl: Duration,
        resolve: F,
    ) -> Option<GeoResolution>
    where
        F: FnOnce(ProxySpec) -> Fut,
        Fut: Future<Output = std::result::Result<GeoResolution, E>>,
    {
        if rotation != ProxyRotation::Sticky {
            return None;
        }
        let key = Self::key(proxy);
        if let Some(hit) = self.get(&key, ttl) {
            return Some(hit);
        }
        let resolved = sanitize(resolve(proxy.clone()).await.ok()?)?;
        self.put(key, resolved.clone());
        Some(resolved)
    }
}

/// Drop an unusable resolution, and an unusable zone from a usable one.
fn sanitize(mut resolution: GeoResolution) -> Option<GeoResolution> {
    if resolution.exit_ip.trim().is_empty() {
        return None; // without an IP there is nothing to pin
    }
    if let Some(tz) = &resolution.timezone {
        if !is_valid_iana_tz(tz) {
            // Keep the IP pin; let the binary derive the zone from it.
            resolution.timezone = None;
        }
    }
    Some(resolution)
}

/// Environment variables that let the browser skip its own geo round trip.
///
/// Merge into the launch environment:
///
/// ```no_run
/// use chromeleon::launch::Launcher;
/// use chromeleon::perf::{sticky_geo_env, GeoResolution, ProxyRotation};
/// use chromeleon::ProxySpec;
///
/// # fn resolve_through(_p: &ProxySpec) -> Result<GeoResolution, std::io::Error> {
/// #     Ok(GeoResolution::new("203.0.113.7", Some("Europe/Warsaw".into())))
/// # }
/// let proxy = ProxySpec::parse("http://user:pass@gw:12321")?;
/// let launcher = Launcher::new("/opt/chromeleon/chrome")
///     .envs(sticky_geo_env(&proxy, ProxyRotation::Sticky, resolve_through));
/// # let _ = launcher;
/// # Ok::<(), chromeleon::Error>(())
/// ```
///
/// `resolve` is yours because the query has to go **through the proxy** and
/// this crate has no HTTP client: one request to an endpoint like
/// `https://ipwho.is/` returns the exit IP and the zone in one round trip. A
/// resolver that cannot fail still has to name an error type for inference —
/// `Ok::<_, std::convert::Infallible>(resolution)` is the usual spelling.
pub fn sticky_geo_env<F, E>(
    proxy: &ProxySpec,
    rotation: ProxyRotation,
    resolve: F,
) -> Vec<(String, String)>
where
    F: FnOnce(&ProxySpec) -> std::result::Result<GeoResolution, E>,
{
    GeoCache::global()
        .resolve_with(proxy, rotation, DEFAULT_GEO_TTL, resolve)
        .map(|r| r.env())
        .unwrap_or_default()
}

/// A pool was asked for with no endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoEndpoints;

impl std::fmt::Display for NoEndpoints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a BrowserPool needs at least one CDP endpoint")
    }
}

impl std::error::Error for NoEndpoints {}

/// Round-robin pool of warm connections to already-running CDP endpoints.
///
/// Launch Chromeleon once per endpoint and connect many times: the pool caches
/// one connection per endpoint and hands out `Arc` clones, so a throughput
/// workload never pays the per-launch tax. Each task should still create its
/// own browser context for isolation and close it when done; the connection
/// stays warm.
///
/// The pool never launches processes — endpoint lifecycle is the operator's.
///
/// ```no_run
/// # use chromeleon::perf::BrowserPool;
/// # #[derive(Debug)] struct Conn;
/// # async fn connect(_e: String) -> Result<Conn, std::io::Error> { Ok(Conn) }
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// let pool = BrowserPool::<Conn>::new(["http://c1:9222", "http://c2:9222"])?;
/// let conn = pool.acquire(connect).await?;   // warm after the first call
/// # let _ = conn;
/// # Ok(()) }
/// ```
pub struct BrowserPool<C> {
    endpoints: Vec<String>,
    next: AtomicUsize,
    /// One slot per endpoint, each with its OWN async lock.
    ///
    /// The map's lock is only ever held for map operations — clone a slot out,
    /// drop the guard — and the connect happens under the slot. A pool-wide
    /// lock held across the caller's `connect` would make one slow endpoint
    /// stall acquires for endpoints that are already warm, which is the exact
    /// opposite of what a pool is for. Same shape as the registration slots in
    /// [`crate::registration`].
    slots: StdMutex<HashMap<String, Slot<C>>>,
}

type Slot<C> = Arc<async_lock::Mutex<Option<Arc<C>>>>;

impl<C> BrowserPool<C> {
    /// Build a pool over one or more CDP endpoints.
    ///
    /// The connection type is only fixed by the first
    /// [`acquire`](BrowserPool::acquire), so at construction it usually needs
    /// naming: `BrowserPool::<Browser>::new([...])`.
    pub fn new<I, S>(endpoints: I) -> std::result::Result<Self, NoEndpoints>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let endpoints: Vec<String> = endpoints.into_iter().map(Into::into).collect();
        if endpoints.is_empty() {
            return Err(NoEndpoints);
        }
        Ok(BrowserPool {
            endpoints,
            next: AtomicUsize::new(0),
            slots: StdMutex::new(HashMap::new()),
        })
    }

    /// The endpoints, in the order they were given.
    pub fn endpoints(&self) -> &[String] {
        &self.endpoints
    }

    /// The next endpoint in the rotation. Advances the cursor.
    pub fn next_endpoint(&self) -> &str {
        let i = self.next.fetch_add(1, Ordering::Relaxed);
        &self.endpoints[i % self.endpoints.len()]
    }

    /// Number of endpoints with a live cached connection.
    ///
    /// Never blocks: an endpoint whose connect is in flight counts as cold,
    /// because it is.
    pub fn warm_count(&self) -> usize {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|slot| slot.try_lock().is_some_and(|held| held.is_some()))
            .count()
    }

    fn slot(&self, endpoint: &str) -> Slot<C> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = slots.get(endpoint) {
            return Arc::clone(existing);
        }
        let created: Slot<C> = Arc::new(async_lock::Mutex::new(None));
        slots.insert(endpoint.to_string(), Arc::clone(&created));
        created
    }

    /// A warm connection for the next endpoint, connecting on first use.
    ///
    /// `connect` runs under **that endpoint's** slot, so two tasks racing on one
    /// cold endpoint make exactly one connection between them, while a slow
    /// connect on one endpoint leaves every other endpoint — and
    /// [`warm_count`](BrowserPool::warm_count) and
    /// [`drain`](BrowserPool::drain) — unaffected.
    ///
    /// Cancelling this future releases the slot; a connect that may hang should
    /// be bounded by the caller, since only the caller knows what "too long"
    /// means for their driver.
    pub async fn acquire<F, Fut, E>(&self, connect: F) -> std::result::Result<Arc<C>, E>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = std::result::Result<C, E>>,
    {
        let endpoint = self.next_endpoint().to_string();
        let slot = self.slot(&endpoint);
        let mut held = slot.lock_arc().await;
        if let Some(existing) = held.as_ref() {
            return Ok(Arc::clone(existing));
        }
        let connection = Arc::new(connect(endpoint).await?);
        *held = Some(Arc::clone(&connection));
        Ok(connection)
    }

    /// Forget a connection — call it when one has gone bad, so the next
    /// [`BrowserPool::acquire`] for that endpoint reconnects.
    ///
    /// Waits only on that endpoint's slot, so an in-flight connect somewhere
    /// else does not delay it.
    pub async fn evict(&self, endpoint: &str) -> Option<Arc<C>> {
        let slot = {
            let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            slots.get(endpoint).map(Arc::clone)
        }?;
        slot.lock_arc().await.take()
    }

    /// Take every connection out of the pool, for shutdown. The pool is empty
    /// afterwards; closing is the caller's, since only they know the driver's
    /// close API.
    ///
    /// These are `Arc`s, so a driver whose `close` takes `&mut self`
    /// (chromiumoxide's does) needs sole ownership: drop the clones you handed
    /// out, then `Arc::into_inner`. Dropping every clone is also a perfectly
    /// good shutdown for a driver that closes on drop.
    pub async fn drain(&self) -> Vec<Arc<C>> {
        // Detach every slot first, so the pool is empty immediately and a
        // concurrent acquire starts a fresh one rather than waiting on us.
        let slots: Vec<Slot<C>> = {
            let mut map = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            map.drain().map(|(_, slot)| slot).collect()
        };
        let mut drained = Vec::with_capacity(slots.len());
        for slot in slots {
            // A slot mid-connect is skipped rather than waited on: it is
            // detached, so whatever it produces is dropped with it.
            if let Some(mut held) = slot.try_lock() {
                drained.extend(held.take());
            }
        }
        drained
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rotating_proxy_is_never_pinned() {
        let proxy = ProxySpec::parse("http://u:p@gw:1").unwrap();
        let env = sticky_geo_env(&proxy, ProxyRotation::Rotating, |_| {
            Err::<GeoResolution, &str>("resolver must not run")
        });
        assert!(env.is_empty());
    }

    #[test]
    fn two_credentials_on_one_gateway_are_two_exits() {
        let cache = GeoCache::new();
        let session_a = ProxySpec::parse("http://user-session-a:pw@gw:12321").unwrap();
        let session_b = ProxySpec::parse("http://user-session-b:pw@gw:12321").unwrap();
        let resolve_to = |ip: &'static str| {
            move |_: &ProxySpec| {
                Ok::<_, std::convert::Infallible>(GeoResolution::new(
                    ip,
                    Some("Europe/Warsaw".to_string()),
                ))
            }
        };
        let a = cache
            .resolve_with(
                &session_a,
                ProxyRotation::Sticky,
                DEFAULT_GEO_TTL,
                resolve_to("203.0.113.1"),
            )
            .unwrap();
        let b = cache
            .resolve_with(
                &session_b,
                ProxyRotation::Sticky,
                DEFAULT_GEO_TTL,
                resolve_to("198.51.100.2"),
            )
            .unwrap();
        assert_eq!(a.exit_ip, "203.0.113.1");
        assert_eq!(
            b.exit_ip, "198.51.100.2",
            "the sticky username selects the exit; a server-keyed cache would \
             have served session A's IP to session B"
        );
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn a_bad_timezone_keeps_the_ip_pin() {
        let cache = GeoCache::new();
        let proxy = ProxySpec::parse("http://u:p@gw:2").unwrap();
        let resolved = cache
            .resolve_with(&proxy, ProxyRotation::Sticky, DEFAULT_GEO_TTL, |_| {
                Ok::<_, &str>(GeoResolution::new("203.0.113.7", Some("Not A Zone".into())))
            })
            .unwrap();
        assert_eq!(resolved.exit_ip, "203.0.113.7");
        assert_eq!(resolved.timezone, None);
        assert_eq!(
            resolved.env(),
            [(EXIT_IP_ENV.to_string(), "203.0.113.7".to_string())]
        );
    }

    #[test]
    fn a_resolver_failure_is_not_a_launch_failure() {
        let cache = GeoCache::new();
        let proxy = ProxySpec::parse("http://u:p@gw:3").unwrap();
        assert!(cache
            .resolve_with(&proxy, ProxyRotation::Sticky, DEFAULT_GEO_TTL, |_| Err::<
                GeoResolution,
                &str,
            >(
                "timeout"
            ))
            .is_none());
        assert!(cache.is_empty()); // and nothing poisonous was cached
    }

    #[test]
    fn timezone_shape_gate() {
        assert!(is_valid_iana_tz("Europe/Warsaw"));
        assert!(is_valid_iana_tz("America/Argentina/Buenos_Aires"));
        assert!(!is_valid_iana_tz("UTC")); // no area
        assert!(!is_valid_iana_tz("Europe/ Warsaw"));
        assert!(!is_valid_iana_tz(""));
        assert!(!is_valid_iana_tz(&format!("Europe/{}", "x".repeat(64))));
    }
}
