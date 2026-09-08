//! The per-context proxy handshake, and nothing else.
//!
//! Chromeleon attaches an authenticated proxy to a browser context with two
//! browser-level CDP commands, in order, on one connection:
//!
//! ```text
//! Target.setProxyCredentials  {proxyServer, username, password}
//! Target.createBrowserContext {proxyServer}
//! ```
//!
//! That is a PROTOCOL fact, not a driver fact. This module knows only the
//! protocol: it parses a proxy into the parts each command is allowed to see,
//! and renders the parameters. It sends nothing and owns no connection — every
//! driver (chromiumoxide, headless_chrome, a raw DevTools WebSocket) is a short
//! adapter on top, and the lock the single-use registration requires lives in
//! [`crate::registration`].
//!
//! The rules encoded here, none of which are visible when you get them wrong:
//!
//! * the `proxyServer` string must be byte-identical in both commands;
//! * credentials go only in the registration, never on the context;
//! * credentials embedded in a URL are percent-encoded and must be DECODED
//!   before they go on the wire, or you authenticate with the wrong secret.
//!
//! Not enforced here, because the binary enforces it: passing credentials at
//! LAUNCH leaves the exit IP unresolved, which unbinds persona geo and disables
//! WebRTC masking. Chromeleon fails closed on that with a remediation banner.

use std::fmt;

use percent_encoding::percent_decode_str;
use serde::Serialize;
use serde_json::Value;
use url::Url;

/// The registration command. Paired with [`ProxySpec::credentials_params`].
pub const CREDENTIALS_METHOD: &str = "Target.setProxyCredentials";

/// The command the registration is consumed by.
pub const CREATE_CONTEXT_METHOD: &str = "Target.createBrowserContext";

/// Everything that can go wrong before a byte reaches the browser.
///
/// Deliberately small and inspectable: each variant is a mistake this client
/// exists to catch, and each one used to be a silent failure (a proxy that
/// authenticates with the wrong secret, or a registration that is never
/// consumed) rather than an error.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The proxy server could not be parsed as a URL, or has no host.
    ///
    /// Raised rather than papered over: a proxy string with a stray character
    /// or a trailing space used to lose its port and come back pointing at
    /// `:80`, which is both wrong and quiet.
    InvalidServer {
        /// The string as given.
        server: String,
        /// What the URL parser objected to.
        reason: String,
    },

    /// A structured proxy was given with no `server`.
    MissingServer,

    /// Preregistration was asked for without a username and password.
    ///
    /// An unauthenticated proxy needs no handshake — pass it straight to the
    /// driver.
    Unauthenticated {
        /// The normalized server that was offered.
        server: String,
    },

    /// Preregistration was asked for on a non-HTTP(S) proxy (e.g. SOCKS).
    UnsupportedScheme {
        /// The normalized server, scheme included.
        server: String,
    },

    /// A credential embedded in the URL percent-decodes to bytes that are not
    /// UTF-8.
    ///
    /// Neither sibling client gets this right and both failure modes are bad:
    /// Python substitutes U+FFFD, which authenticates with a secret that is not
    /// yours and fails silently at the proxy; Node throws `URI malformed`,
    /// which is loud but does not say what was wrong. Pass such a credential as
    /// an explicit field instead of embedding it in the URL.
    UndecodableCredential {
        /// `"username"` or `"password"`.
        field: &'static str,
    },

    /// A value longer than the browser will accept.
    ///
    /// The browser bounds the server at 2048 bytes and each credential at 1024,
    /// and answers an over-long one with `InvalidParams`. Checking here turns
    /// that into an error that names the field.
    TooLong {
        /// `"server"`, `"username"` or `"password"`.
        field: &'static str,
        /// The length that was offered.
        len: usize,
        /// The largest the browser accepts.
        max: usize,
    },

    /// The browser refused `Target.setProxyCredentials`.
    RegistrationRefused {
        /// The `error` member the browser returned.
        detail: String,
    },

    /// `Page.getFrameTree` named no main frame, so a
    /// [`SettleWatch`](crate::settle::SettleWatch) cannot tell the main frame's
    /// lifecycle from a subframe's.
    ///
    /// Refused rather than guessed, and refused at ARM time, before anything is
    /// navigated: `bbc.com/news` emits lifecycle from 23 frames, and a watch
    /// that counts the first of them reports `networkIdle` at 699ms for a page
    /// whose real value is 6094ms.
    MissingMainFrame {
        /// What the session answered instead.
        detail: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidServer { server, reason } => {
                write!(f, "unparseable proxy server {server:?}: {reason}")
            }
            Error::MissingServer => f.write_str("proxy requires a server"),
            Error::Unauthenticated { server } => write!(
                f,
                "per-context preregistration needs a username and password; an \
                 unauthenticated proxy ({server}) can be passed straight to the driver"
            ),
            Error::UnsupportedScheme { server } => write!(
                f,
                "credential preregistration requires an HTTP(S) proxy, got {server}"
            ),
            Error::UndecodableCredential { field } => write!(
                f,
                "the {field} in the proxy URL percent-decodes to bytes that are not UTF-8; \
                 pass it as an explicit credential instead of embedding it in the URL"
            ),
            Error::TooLong { field, len, max } => {
                write!(
                    f,
                    "{field} is {len} bytes; the browser accepts at most {max}"
                )
            }
            Error::RegistrationRefused { detail } => {
                write!(f, "proxy credential registration refused: {detail}")
            }
            Error::MissingMainFrame { detail } => {
                write!(
                    f,
                    "this page session named no main frame, so main-frame and subframe \
                     lifecycle cannot be told apart: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

/// Result of anything in this crate that can only fail the ways [`Error`] does.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A proxy split into the parts each command is allowed to see.
///
/// The only ways to build one normalize the server ([`ProxySpec::parse`],
/// [`ProxySpec::new`]), so a `ProxySpec` in hand is a promise that the string
/// you register and the string the context is created with are the same bytes.
/// The other clients keep that invariant by convention; here it is the type.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ProxySpec {
    server: String,
    username: Option<String>,
    password: Option<String>,
}

/// Redacted on purpose: a proxy password in a log line is a leaked credential,
/// and `{:?}` on a struct is how it gets there. The presence of a password is
/// still visible, because "did it parse one?" is the question you are usually
/// debugging.
impl fmt::Debug for ProxySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxySpec")
            .field("server", &self.server)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl ProxySpec {
    /// Build a spec from separate parts, normalizing the server.
    ///
    /// Credentials given separately are LITERAL — they are never
    /// percent-decoded, because nothing percent-encoded them. Only credentials
    /// that arrived inside a URL are decoded (see [`ProxySpec::parse`]).
    ///
    /// When **neither** credential is supplied and the server itself carries
    /// `user:pass@`, those credentials are taken from it, decoded. The carrier
    /// — a string or a struct — is not a licence to drop them, and dropping
    /// them is not a safe default: it produces a spec that looks fine and
    /// cannot be registered. (The Node client does drop them here; this follows
    /// Python, which is right.)
    pub fn new(
        server: impl AsRef<str>,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self> {
        let username = username.map(str::to_string);
        let password = password.map(str::to_string);
        let server = server.as_ref();
        if username.is_none() && password.is_none() && server.contains('@') {
            return ProxySpec::parse(server);
        }
        Ok(ProxySpec {
            server: normalize_server(server)?,
            username,
            password,
        })
    }

    /// Parse a proxy URL, with or without embedded `user:pass@`.
    ///
    /// ```
    /// # use chromeleon::ProxySpec;
    /// let spec = ProxySpec::parse("http://user:p%40ss@GW.example.com:12321")?;
    /// assert_eq!(spec.server(), "http://gw.example.com:12321");
    /// assert_eq!(spec.password(), Some("p@ss"));   // %40 decoded
    /// # Ok::<(), chromeleon::Error>(())
    /// ```
    pub fn parse(url: &str) -> Result<Self> {
        let raw = url.trim();
        let scheme_len = scheme_prefix_len(raw);
        let rest = &raw[scheme_len..];
        // Only the AUTHORITY may carry credentials. Searching the whole string
        // for the last '@' finds one in a path or query and truncates the host.
        let authority_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_len];

        let (username, password, server) = match authority.rfind('@') {
            None => (None, None, raw.to_string()),
            Some(at) => {
                let cred = &authority[..at];
                // Credentials inside a URL are percent-encoded by definition —
                // a password containing '@' or ':' has to be. Sending them
                // still encoded authenticates with the wrong secret, and the
                // proxy's 407 surfaces as a page that will not load.
                let (user, pass) = match cred.find(':') {
                    None => (decode(cred, "username")?, None),
                    Some(colon) => (
                        decode(&cred[..colon], "username")?,
                        Some(decode(&cred[colon + 1..], "password")?),
                    ),
                };
                let mut server = String::with_capacity(raw.len());
                server.push_str(&raw[..scheme_len]);
                server.push_str(&raw[scheme_len + at + 1..]);
                (Some(user), pass, server)
            }
        };

        Ok(ProxySpec {
            server: normalize_server(&server)?,
            username,
            password,
        })
    }

    /// The normalized `scheme://host[:port]`, byte-identical in both commands.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The username, decoded, or `None` when the proxy carried no userinfo.
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    /// The password, decoded. `None` means "no password given"; `Some("")` is
    /// an explicit empty password, which is a real thing some gateways use.
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    /// A non-empty username AND a password was given (even an empty one).
    ///
    /// This is the gate on preregistration: without both there is nothing to
    /// register, and the proxy belongs straight on the driver instead.
    pub fn authenticated(&self) -> bool {
        self.username.as_deref().is_some_and(|u| !u.is_empty()) && self.password.is_some()
    }

    /// Params for `Target.setProxyCredentials`, as the wire sees them.
    ///
    /// Absent credentials render as empty strings, matching the other clients.
    pub fn credentials_params(&self) -> CredentialsParams<'_> {
        CredentialsParams {
            proxy_server: &self.server,
            username: self.username.as_deref().unwrap_or(""),
            password: self.password.as_deref().unwrap_or(""),
            credentials_id: None,
        }
    }

    /// Params for `Target.createBrowserContext`.
    ///
    /// Use this, not a server string of your own: the registration is matched
    /// by bytes and is consumed by exactly this call.
    pub fn create_context_params(&self) -> CreateContextParams<'_> {
        CreateContextParams {
            proxy_server: &self.server,
            proxy_credentials_id: None,
        }
    }

    /// Everything preregistration requires, as one call.
    ///
    /// [`crate::registration`] runs this for you; it is public so a driver that
    /// takes its own path can refuse the same inputs. The length bounds mirror
    /// the browser's own, so an over-long credential is an error that names the
    /// field instead of an `InvalidParams` from the far end.
    pub fn check_registrable(&self) -> Result<()> {
        if !self.authenticated() {
            return Err(Error::Unauthenticated {
                server: self.server.clone(),
            });
        }
        if !(self.server.starts_with("http://") || self.server.starts_with("https://")) {
            return Err(Error::UnsupportedScheme {
                server: self.server.clone(),
            });
        }
        check_len("server", self.server.len(), MAX_SERVER_BYTES)?;
        check_len(
            "username",
            self.username.as_deref().unwrap_or("").len(),
            MAX_CREDENTIAL_BYTES,
        )?;
        check_len(
            "password",
            self.password.as_deref().unwrap_or("").len(),
            MAX_CREDENTIAL_BYTES,
        )?;
        Ok(())
    }
}

/// Params for `Target.setProxyCredentials`.
///
/// `Debug` redacts the password; the wire form from
/// [`CredentialsParams::to_value`] does not, because that is the wire.
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsParams<'a> {
    /// The normalized server, byte-identical to the context's.
    pub proxy_server: &'a str,
    /// The decoded username, or `""` when there is none.
    pub username: &'a str,
    /// The decoded password, or `""` when there is none.
    pub password: &'a str,
    /// Optional correlation token — see [`CredentialsParams::with_credentials_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials_id: Option<&'a str>,
}

/// Params for `Target.createBrowserContext`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateContextParams<'a> {
    /// The normalized server, byte-identical to the registration's.
    pub proxy_server: &'a str,
    /// Optional correlation token — see [`CreateContextParams::with_credentials_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_credentials_id: Option<&'a str>,
}

impl fmt::Debug for CredentialsParams<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialsParams")
            .field("proxy_server", &self.proxy_server)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("credentials_id", &self.credentials_id)
            .finish()
    }
}

impl<'a> CredentialsParams<'a> {
    /// Tag this registration so a specific context can claim it.
    ///
    /// ⚠️ **Opt-in, and version-gated.** `credentialsId` exists from v151.5; an
    /// older binary — including the v151.4 currently published as `latest` —
    /// does not know the field. Give the same token to
    /// [`CreateContextParams::with_credentials_id`]; the two must agree or the
    /// context will not find its registration.
    ///
    /// It buys concurrency at the pool. Without a token the browser serializes
    /// same-endpoint registrations itself by DEFERRING the reply until the slot
    /// frees; a tokened registration never queues, because it is independent —
    /// which is the whole point of the field. This client keeps its own lock
    /// either way, because it cannot know which binary it is talking to, and on
    /// v151.4 a duplicate is rejected outright rather than queued.
    pub fn with_credentials_id(mut self, id: &'a str) -> Self {
        self.credentials_id = Some(id);
        self
    }

    /// The same params as a `serde_json::Value`, for drivers that take one.
    pub fn to_value(self) -> Value {
        let mut params = serde_json::json!({
            "proxyServer": self.proxy_server,
            "username": self.username,
            "password": self.password,
        });
        if let Some(id) = self.credentials_id {
            params["credentialsId"] = Value::String(id.to_string());
        }
        params
    }
}

impl<'a> CreateContextParams<'a> {
    /// Claim the registration tagged with this token. See
    /// [`CredentialsParams::with_credentials_id`].
    pub fn with_credentials_id(mut self, id: &'a str) -> Self {
        self.proxy_credentials_id = Some(id);
        self
    }

    /// The same params as a `serde_json::Value`, for drivers that take one.
    pub fn to_value(self) -> Value {
        let mut params = serde_json::json!({ "proxyServer": self.proxy_server });
        if let Some(id) = self.proxy_credentials_id {
            params["proxyCredentialsId"] = Value::String(id.to_string());
        }
        params
    }
}

/// Bounds the browser enforces on `Target.setProxyCredentials`.
const MAX_SERVER_BYTES: usize = 2048;
const MAX_CREDENTIAL_BYTES: usize = 1024;

impl std::str::FromStr for ProxySpec {
    type Err = Error;

    fn from_str(url: &str) -> Result<Self> {
        ProxySpec::parse(url)
    }
}

impl TryFrom<&str> for ProxySpec {
    type Error = Error;

    fn try_from(url: &str) -> Result<Self> {
        ProxySpec::parse(url)
    }
}

/// The normalized server. Never the credentials — see the `Debug` impl.
impl fmt::Display for ProxySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.server)
    }
}

/// Anything that can be read as a proxy: a URL string, a `ProxySpec`, or a
/// `(server, username, password)` tuple.
///
/// ```
/// # use chromeleon::{IntoProxySpec, ProxySpec};
/// let a = "http://u:p@gw:12321".into_proxy_spec()?;
/// let b = ("gw:12321", "u", "p").into_proxy_spec()?;
/// assert_eq!(a, b);                       // same server, same credentials
/// # Ok::<(), chromeleon::Error>(())
/// ```
pub trait IntoProxySpec {
    /// Parse and normalize into a [`ProxySpec`].
    fn into_proxy_spec(self) -> Result<ProxySpec>;
}

impl IntoProxySpec for ProxySpec {
    fn into_proxy_spec(self) -> Result<ProxySpec> {
        Ok(self)
    }
}

impl IntoProxySpec for &ProxySpec {
    fn into_proxy_spec(self) -> Result<ProxySpec> {
        Ok(self.clone())
    }
}

impl IntoProxySpec for &str {
    fn into_proxy_spec(self) -> Result<ProxySpec> {
        ProxySpec::parse(self)
    }
}

impl IntoProxySpec for &String {
    fn into_proxy_spec(self) -> Result<ProxySpec> {
        ProxySpec::parse(self)
    }
}

impl IntoProxySpec for String {
    fn into_proxy_spec(self) -> Result<ProxySpec> {
        ProxySpec::parse(&self)
    }
}

impl<S, U, P> IntoProxySpec for (S, U, P)
where
    S: AsRef<str>,
    U: Into<String>,
    P: Into<String>,
{
    fn into_proxy_spec(self) -> Result<ProxySpec> {
        let username = self.1.into();
        let password = self.2.into();
        ProxySpec::new(self.0, Some(&username), Some(&password))
    }
}

/// Canonicalise a proxy server the way a WHATWG URL parser will.
///
/// Byte-identity between the two commands is required, and in the general case
/// only one of them is yours: **Playwright** rewrites the server it sends to
/// `createBrowserContext` as `url.protocol + "//" + url.host`, which lowercases
/// the host, drops the scheme's default port, and prepends `http://` when the
/// scheme is missing. Register the raw string and it no longer matches the
/// context's, so the registration is never consumed — a browser that silently
/// makes direct requests.
///
/// Drivers that pass the string through verbatim (chromiumoxide does) will not
/// rescue you either: then the *browser* rejects the pair, because it matches
/// the two by exact string. Normalizing first is what makes both cases work.
///
/// ```
/// # use chromeleon::normalize_server;
/// assert_eq!(normalize_server("GATEWAY.example.com:12321")?, "http://gateway.example.com:12321");
/// assert_eq!(normalize_server("http://gw.example.com:80")?,  "http://gw.example.com");
/// assert_eq!(normalize_server("https://gw:443")?,            "https://gw");
/// # Ok::<(), chromeleon::Error>(())
/// ```
///
/// Errors on an unparseable port or a missing host rather than guessing.
pub fn normalize_server(server: &str) -> Result<String> {
    let raw = server.trim();
    if raw.is_empty() {
        // Distinguished from InvalidServer on purpose: nothing was given, which
        // is a caller mistake with a different fix than a malformed string. The
        // Python client turns this into the server `"http://"`, which survives
        // its own normalization and then arrives at the browser as the host
        // `http` — a proxy that silently is not the one you meant.
        return Err(Error::MissingServer);
    }
    let scheme_len = scheme_prefix_len(raw);
    let with_scheme = if scheme_len > 0 {
        raw.to_string()
    } else {
        // A `://` that our scheme scan did not recognise means something is in
        // front of it — a BOM, a stray quote, a copied bullet. Prepending
        // `http://` would parse the whole thing as the host `http` and drop the
        // real one, which is the silent-wrong-proxy failure this crate exists
        // to prevent.
        if raw.contains("://") {
            return Err(Error::InvalidServer {
                server: redact_userinfo(raw),
                reason: "a scheme is present but unreadable — check for a leading \
                         invisible character"
                    .to_string(),
            });
        }
        format!("http://{raw}")
    };
    let url = Url::parse(&with_scheme).map_err(|e| Error::InvalidServer {
        server: redact_userinfo(server),
        reason: e.to_string(),
    })?;
    let host = url.host_str().ok_or_else(|| Error::InvalidServer {
        server: redact_userinfo(server),
        reason: "no host".to_string(),
    })?;
    if host.is_empty() {
        return Err(Error::InvalidServer {
            server: redact_userinfo(server),
            reason: "empty host".to_string(),
        });
    }
    // `url` follows WHATWG here exactly as `new URL()` does in the Node client:
    // the host is already lowercased, percent-decoded, IDNA-encoded and (for
    // IPv6) compressed and bracketed, and `port()` is None for the scheme's
    // default port. That agreement is the whole point of taking the dependency.
    Ok(match url.port() {
        Some(port) => format!("{}://{}:{}", url.scheme(), host, port),
        None => format!("{}://{}", url.scheme(), host),
    })
}

/// Parse anything [`IntoProxySpec`] accepts.
///
/// A free function for symmetry with the Python (`parse_proxy`) and Node
/// (`parseProxy`) clients; [`ProxySpec::parse`] is the same thing for strings.
pub fn parse_proxy(proxy: impl IntoProxySpec) -> Result<ProxySpec> {
    proxy.into_proxy_spec()
}

/// Raise if the browser refused the registration.
///
/// A typed driver usually turns a CDP error into its own error before you get
/// here; a raw WebSocket hands you the envelope, and an `error` member in it is
/// a refusal however successful the send looked.
pub fn check_registration(result: &Value) -> Result<()> {
    match result.get("error") {
        Some(err) if !err.is_null() => Err(Error::RegistrationRefused {
            detail: err.to_string(),
        }),
        _ => Ok(()),
    }
}

/// Mask any `user:pass@` before a server string goes into an error.
///
/// An unparseable server is reported back to the caller and usually logged;
/// when the parse failed it may still have carried credentials, and an error
/// message is the last place a password should turn up.
fn redact_userinfo(server: &str) -> String {
    // Scope to the authority the same way parsing does. Looking for the path
    // delimiter in the WHOLE string finds the `//` of the scheme first, which
    // is exactly the mistake this crate refuses to make when splitting
    // credentials — and it silently disables the redaction.
    let scheme_len = scheme_prefix_len(server);
    let rest = &server[scheme_len..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{}<redacted>@{}", &server[..scheme_len], &rest[at + 1..]),
        None => server.to_string(),
    }
}

fn check_len(field: &'static str, len: usize, max: usize) -> Result<()> {
    if len > max {
        return Err(Error::TooLong { field, len, max });
    }
    Ok(())
}

/// Length of a leading `scheme://`, or 0 when there is none.
///
/// Matches the Node client's `/^[a-z][a-z0-9+.-]*:\/\//i`. A bare `host:port`
/// must NOT be read as a scheme, which is why this insists on the `//`.
fn scheme_prefix_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return 0;
    }
    let mut i = 1;
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'+' | b'-' | b'.'))
    {
        i += 1;
    }
    if s[i..].starts_with("://") {
        i + 3
    } else {
        0
    }
}

/// Percent-decode a credential, WHATWG-style.
///
/// Two deliberate positions, both taken from measuring the sibling clients
/// against the spec:
///
/// * An invalid or truncated escape passes through as itself. JS
///   `decodeURIComponent` throws on a stray `%`, which turns a password like
///   `p%ssword` — an ordinary password — into a hard failure. A literal `%` is
///   far more common than a mistyped escape.
/// * A well-formed escape whose bytes are not UTF-8 is an ERROR, not a
///   replacement character. Substituting U+FFFD is the exact failure invariant
///   2 warns about: you authenticate with a secret that is not yours, and the
///   proxy's refusal shows up as a page that will not load.
fn decode(s: &str, field: &'static str) -> Result<String> {
    percent_decode_str(s)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .map_err(|_| Error::UndecodableCredential { field })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_prefix_needs_the_slashes() {
        assert_eq!(scheme_prefix_len("http://gw"), 7);
        assert_eq!(scheme_prefix_len("HTTP://gw"), 7);
        assert_eq!(scheme_prefix_len("socks5://gw"), 9);
        assert_eq!(scheme_prefix_len("gw:12321"), 0); // a port, not a scheme
        assert_eq!(scheme_prefix_len("1http://gw"), 0);
        assert_eq!(scheme_prefix_len(""), 0);
    }

    #[test]
    fn credentials_render_empty_when_absent() {
        let spec = ProxySpec::parse("http://gw:12321").unwrap();
        let params = spec.credentials_params();
        assert_eq!(params.username, "");
        assert_eq!(params.password, "");
        assert_eq!(params.proxy_server, "http://gw:12321");
    }

    #[test]
    fn an_invisible_character_before_the_scheme_is_an_error_not_a_new_host() {
        // A BOM survives `trim`, so the scheme scan fails and the whole string
        // would become the host: WHATWG maps the format character away and you
        // are left proxying through "http". Refuse instead.
        let bom = ProxySpec::parse("\u{feff}http://gw.example.com:12321");
        assert!(matches!(bom, Err(Error::InvalidServer { .. })), "{bom:?}");
        // The same for a stray quote or bullet pasted from a document.
        assert!(ProxySpec::parse("\"http://gw:1\"").is_err());
        // …while an ordinary scheme-less proxy is still fine.
        assert_eq!(
            ProxySpec::parse("gw.example.com:12321").unwrap().server(),
            "http://gw.example.com:12321"
        );
    }

    #[test]
    fn a_password_never_reaches_a_log_line() {
        let spec = ProxySpec::parse("http://user:hunter2@gw:12321").unwrap();
        let rendered = format!("{spec:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("user"), "the username is not the secret");
        // …and the same for the params struct that carries it to the wire.
        let params = format!("{:?}", spec.credentials_params());
        assert!(!params.contains("hunter2"), "{params}");
        // The WIRE form still carries it, or nothing would authenticate.
        assert_eq!(spec.credentials_params().to_value()["password"], "hunter2");
    }

    #[test]
    fn an_unparseable_server_does_not_echo_its_credentials() {
        // Through `parse` the userinfo is already split off before normalization,
        // so the error cannot carry it…
        let via_parse = ProxySpec::parse("http://user:hunter2@gw:notaport")
            .unwrap_err()
            .to_string();
        assert!(!via_parse.contains("hunter2"), "{via_parse}");

        // …but `normalize_server` is public and takes whole URLs, and its error
        // gets logged like any other.
        let direct = normalize_server("http://user:hunter2@gw:notaport")
            .unwrap_err()
            .to_string();
        assert!(!direct.contains("hunter2"), "{direct}");
        assert!(direct.contains("<redacted>"), "{direct}");
    }

    #[test]
    fn check_registration_only_trips_on_an_error_member() {
        assert!(check_registration(&serde_json::json!({})).is_ok());
        assert!(check_registration(&serde_json::json!({"error": null})).is_ok());
        assert!(check_registration(&serde_json::json!({"browserContextId": "x"})).is_ok());
        assert!(matches!(
            check_registration(&serde_json::json!({"error": {"code": -32000}})),
            Err(Error::RegistrationRefused { .. })
        ));
    }
}
