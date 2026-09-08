//! Launching the binary: the flags and the environment Chromeleon needs and a
//! driver cannot know about.
//!
//! This is not part of the handshake, but a per-context proxy does not work
//! without it:
//!
//! * **Never pass proxy credentials at launch.** The binary fails closed on an
//!   unresolved exit IP (unbound geo, disabled WebRTC mask) with a remediation
//!   banner. Per-context proxies are launched with no `--proxy-server` at all.
//! * **Merge the WebRTC policy flag** ([`LAUNCH_ARGS`]). A context-level proxy
//!   routes HTTP, but WebRTC's UDP socket is browser-level and would otherwise
//!   gather ICE over the real route — the leak is invisible from the page.
//! * **Strip the controller's `PROXY_*` environment.** Those variables are the
//!   controller's; inheriting them routes browser traffic somewhere you did not
//!   choose.
//!
//! Two features are OPT-IN because they change what the browser does, and both
//! are switched on here rather than in [`LAUNCH_ARGS`]: the captcha solver
//! ([`Launcher::captcha`]) and page-settle
//! ([`Launcher::page_settle`], which registers `Chromeleon.waitForSettle` —
//! without it the command is an unknown method, see [`crate::settle`]).
//!
//! [`Launcher`] gives you a `std::process::Command` with all of that applied,
//! which suits the way Rust drives browsers: spawn Chromeleon with
//! `--remote-debugging-port` and attach over CDP. If your driver spawns the
//! process itself, use [`launch_args`] and [`browser_process_env`] to feed it.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Launch flags a per-context proxy needs.
///
/// A context-level proxy routes HTTP, but WebRTC's UDP socket is browser-level
/// and would otherwise gather ICE over the real route. Chromeleon auto-appends
/// this for LAUNCH-time proxies only, which per-context proxies by definition
/// are not.
pub const LAUNCH_ARGS: &[&str] = &["--webrtc-ip-handling-policy=disable_non_proxied_udp"];

/// Driver default switches worth *not* adding.
///
/// Playwright launches Chromium with ~46 flags of its own; chromiumoxide and
/// Puppeteer ship a similar list. Suppressing this subset is worth about 22
/// points of Google pass rate.
///
/// Measured 2026-08-12 over 36 complete quads on 36 distinct fresh exits, one
/// exit per quad, arm order rotated. A driver-launched Chromeleon carrying a
/// customer's own flags passed 4/36 (11.1%); the **same flags plus this
/// suppression list** passed 12/36 (33.3%) — discordant 8-0, McNemar p = 0.0078,
/// +22.2 pp, 95% CI [+7, +38]. A bare `chrome --proxy-server` launch also scored
/// 12/36 and tied the suppressed arm pair-for-pair (1-1, p = 1.00), which is the
/// load-bearing detail: **the variable is these switches, not the launcher.**
/// An earlier reading that blamed the launch path itself was measured with both
/// arms in the same suppression state, and is retracted.
///
/// A bisection on a high-baseline exit (~100% control, the only place a
/// single-flag drop is measurable at all) isolated exactly two of the thirteen:
/// `--disable-field-trial-config` and
/// `--disable-component-extensions-with-background-pages`, each 9-0, p = 0.0039.
/// Both are things real Chrome does specifically *for Google* — the variations
/// seed behind the `X-Client-Data` header, and Chrome's own component extensions
/// — so their absence is a Google-shaped tell. The other eleven measured inert
/// on that baseline; they stay because suppressing them costs nothing and the
/// set is what the controlled contrast actually tested.
///
/// [`Launcher`] never adds these, because it never adds a default list at all —
/// this is here for adapters over drivers that do.
pub const SUPPRESSED_DEFAULT_ARGS: &[&str] = &[
    "--disable-field-trial-config",
    "--disable-component-update",
    "--disable-client-side-phishing-detection",
    "--metrics-recording-only",
    "--disable-breakpad",
    "--no-service-autorun",
    "--disable-extensions",
    "--disable-default-apps",
    "--disable-component-extensions-with-background-pages",
    "--disable-search-engine-choice-screen",
    "--no-default-browser-check",
    "--unsafely-disable-devtools-self-xss-warnings",
    "--use-mock-keychain",
];

/// True when `arg` is one of [`SUPPRESSED_DEFAULT_ARGS`], compared by switch
/// name so `--foo=1` matches `--foo`.
pub fn is_suppressed_default_arg(arg: &str) -> bool {
    let name = switch_name(arg);
    SUPPRESSED_DEFAULT_ARGS
        .iter()
        .any(|s| switch_name(s) == name)
}

/// A driver's default argument list with [`SUPPRESSED_DEFAULT_ARGS`] removed.
///
/// For drivers whose "use my own defaults" escape hatch is all-or-nothing:
/// take their list, filter it, hand it back.
pub fn without_suppressed_defaults<'a, I>(defaults: I) -> Vec<String>
where
    I: IntoIterator,
    I::Item: AsRef<str> + 'a,
{
    defaults
        .into_iter()
        .filter(|a| !is_suppressed_default_arg(a.as_ref()))
        .map(|a| a.as_ref().to_string())
        .collect()
}

/// `--flag=value` → `--flag`; `--flag` → `--flag`.
fn switch_name(arg: &str) -> &str {
    match arg.find('=') {
        Some(i) => &arg[..i],
        None => arg,
    }
}

/// Merge `extra` into `args`, keeping any switch the caller already set.
///
/// A flag you set yourself always wins — including when you set it to a
/// different value, which is the case that matters: a caller who deliberately
/// picks a different `--webrtc-ip-handling-policy` must not end up with both.
pub fn merge_launch_args<A, E>(args: A, extra: E) -> Vec<String>
where
    A: IntoIterator,
    A::Item: AsRef<str>,
    E: IntoIterator,
    E::Item: AsRef<str>,
{
    let mut merged: Vec<String> = args.into_iter().map(|a| a.as_ref().to_string()).collect();
    for flag in extra {
        let flag = flag.as_ref();
        if !merged.iter().any(|a| switch_name(a) == switch_name(flag)) {
            merged.push(flag.to_string());
        }
    }
    merged
}

/// [`LAUNCH_ARGS`] merged into `args`, plus the captcha switches when asked.
///
/// The one call a driver-owned launch needs:
///
/// ```
/// # use chromeleon::launch::launch_args;
/// let args = launch_args(["--headless=new"], false, None);
/// assert_eq!(args, ["--headless=new", "--webrtc-ip-handling-policy=disable_non_proxied_udp"]);
/// ```
pub fn launch_args<A>(args: A, captcha: bool, captcha_model_path: Option<&str>) -> Vec<String>
where
    A: IntoIterator,
    A::Item: AsRef<str>,
{
    let mut extra: Vec<String> = LAUNCH_ARGS.iter().map(|s| s.to_string()).collect();
    if captcha || captcha_model_path.is_some() {
        extra.extend(crate::captcha::captcha_launch_args(captcha_model_path));
    }
    merge_launch_args(args, extra)
}

/// The process environment with the controller's proxy variables removed.
///
/// `HTTP_PROXY` / `PROXY_*` in the controller's environment are for the
/// CONTROLLER. Inheriting them routes browser traffic somewhere you did not
/// choose, and the symptom is a persona whose exit IP is not the one you paid
/// for.
pub fn browser_process_env() -> BTreeMap<String, String> {
    // `env::vars()` PANICS on a variable that is not UTF-8, and a browser
    // launcher is the last place that should take a process down. Read the OS
    // strings and drop what cannot be represented.
    filter_proxy_env(
        std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))),
    )
}

/// The same filter applied to an environment you supply.
///
/// Any variable whose NAME contains `PROXY`, in any case, is dropped — the same
/// rule the Python and Node clients apply. It is deliberately blunt: this is a
/// filter for a variable you did not mean to pass on, and a variable you did
/// mean to pass on belongs on the `Command` directly.
pub fn filter_proxy_env<I, K, V>(env: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    env.into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .filter(|(k, _)| !k.to_uppercase().contains("PROXY"))
        .collect()
}

/// A `std::process::Command` for Chromeleon, with the flags and environment a
/// per-context proxy needs.
///
/// ```no_run
/// # use chromeleon::launch::Launcher;
/// let mut child = Launcher::new("/opt/chromeleon/chrome")
///     .remote_debugging_port(9222)
///     .arg("--headless=new")
///     .captcha(true)
///     .spawn()?;
/// # let _ = child.kill();
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// The DevTools endpoint is then `http://127.0.0.1:9222`, and the WebSocket URL
/// it advertises is what you hand to your driver — and to
/// [`ConnKey::new`](crate::registration::ConnKey::new).
#[derive(Debug, Clone)]
pub struct Launcher {
    executable: PathBuf,
    args: Vec<String>,
    env_overrides: BTreeMap<String, String>,
    inherit_env: bool,
    captcha: bool,
    captcha_model_path: Option<String>,
    page_settle: Option<crate::settle::SettleTuning>,
    debugging_port: Option<u16>,
}

impl Launcher {
    /// Start from the path to the Chromeleon binary.
    pub fn new(executable: impl AsRef<Path>) -> Self {
        Launcher {
            executable: executable.as_ref().to_path_buf(),
            args: Vec::new(),
            env_overrides: BTreeMap::new(),
            inherit_env: true,
            captcha: false,
            captcha_model_path: None,
            page_settle: None,
            debugging_port: None,
        }
    }

    /// Add one flag. A flag you set here beats anything this crate would add.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add several flags.
    pub fn args<I>(mut self, args: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Expose CDP on `port` (`--remote-debugging-port`).
    ///
    /// Remembered, so [`Launcher::spawn_and_wait`] knows where to look.
    pub fn remote_debugging_port(mut self, port: u16) -> Self {
        self.debugging_port = Some(port);
        self.arg(format!("--remote-debugging-port={port}"))
    }

    /// Use a specific profile directory (`--user-data-dir`).
    pub fn user_data_dir(self, dir: impl AsRef<Path>) -> Self {
        self.arg(format!("--user-data-dir={}", dir.as_ref().display()))
    }

    /// Choose the persona operating system (`--fingerprint-os`).
    ///
    /// ⚠️ Load-bearing for per-context proxies, and easy to miss: the persona OS
    /// is read from the browser's **command line** when a context is created,
    /// and it defaults to `Windows` when neither `--fingerprint-os` nor
    /// `--fingerprint-platform` is set. A launcher that never sets it ships
    /// Windows personas, whatever the host is, without saying so. Values:
    /// `Windows`, `macOS`, `Linux`, `Android`.
    pub fn fingerprint_os(self, os: impl Into<String>) -> Self {
        self.arg(format!("--fingerprint-os={}", os.into()))
    }

    /// Pin the persona (`--fingerprint-seed`), so a run is reproducible.
    ///
    /// Without it every launch generates a fresh persona, which is what you
    /// want in production and exactly what makes a fingerprint comparison
    /// between two runs meaningless.
    pub fn fingerprint_seed(self, seed: u64) -> Self {
        self.arg(format!("--fingerprint-seed={seed}"))
    }

    /// Deliver a full persona from a file (`--cr-cfg-file`).
    ///
    /// Preferred over the inline `--cr-cfg=<json>` form for anything large.
    pub fn config_file(self, path: impl AsRef<Path>) -> Self {
        self.arg(format!("--cr-cfg-file={}", path.as_ref().display()))
    }

    /// Turn on the built-in reCAPTCHA/hCaptcha solver.
    pub fn captcha(mut self, on: bool) -> Self {
        self.captcha = on;
        self
    }

    /// Point the solver at a model directory. Dev/self-host only — the release
    /// binary embeds the models. Implies [`Launcher::captcha`].
    pub fn captcha_model_path(mut self, dir: impl Into<String>) -> Self {
        self.captcha_model_path = Some(dir.into());
        self
    }

    /// Register the page-settle CDP domain (`--page-settle`).
    ///
    /// ⚠️ Without it `Chromeleon.waitForSettle` is not merely off — the domain
    /// is never registered, so the command answers `-32601`, *method not
    /// found*, and [`SettleWatch`](crate::settle::SettleWatch) falls back to
    /// Blink's `networkAlmostIdle`. Through a proxy that fallback never fires
    /// on 36% of loads, so a fleet that forgets this switch is a fleet running
    /// on the weaker signal.
    ///
    /// ```
    /// # use chromeleon::launch::Launcher;
    /// let args = Launcher::new("/opt/chromeleon/chrome").page_settle(true).build_args();
    /// assert!(args.contains(&"--page-settle".to_string()));
    /// ```
    pub fn page_settle(mut self, on: bool) -> Self {
        self.page_settle = on.then(|| self.page_settle.unwrap_or_default());
        self
    }

    /// Register page-settle and tune it. Implies [`Launcher::page_settle`].
    ///
    /// ```
    /// # use chromeleon::launch::Launcher;
    /// # use chromeleon::settle::SettleTuning;
    /// let args = Launcher::new("/opt/chromeleon/chrome")
    ///     .page_settle_tuning(SettleTuning::new().quiet_window_ms(1500).pierce_shadow(true))
    ///     .build_args();
    /// assert!(args.contains(&"--page-settle-quiet-window-ms=1500".to_string()));
    /// ```
    pub fn page_settle_tuning(mut self, tuning: crate::settle::SettleTuning) -> Self {
        self.page_settle = Some(tuning);
        self
    }

    /// Set one environment variable for the browser process.
    ///
    /// Note that the `PROXY_*` filter applies to these too — see
    /// [`filter_proxy_env`]. To set one deliberately, use
    /// [`Launcher::command`] and put it on the `Command`.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_overrides.insert(key.into(), value.into());
        self
    }

    /// Set several environment variables.
    pub fn envs<I, K, V>(mut self, env: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env_overrides
            .extend(env.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// Start from an empty environment instead of the controller's.
    pub fn clear_env(mut self) -> Self {
        self.inherit_env = false;
        self
    }

    /// The full argument list, in the order the browser will see it.
    pub fn build_args(&self) -> Vec<String> {
        let args = launch_args(
            self.args.iter().map(String::as_str),
            self.captcha,
            self.captcha_model_path.as_deref(),
        );
        // Merged rather than appended, so a `--page-settle-*` switch the caller
        // set by hand still wins — the same rule every other flag here follows.
        match self.page_settle {
            Some(tuning) => merge_launch_args(args, crate::settle::settle_launch_args(tuning)),
            None => args,
        }
    }

    /// The full environment the browser will run with.
    pub fn build_env(&self) -> BTreeMap<String, String> {
        let mut env = if self.inherit_env {
            browser_process_env()
        } else {
            BTreeMap::new()
        };
        env.extend(filter_proxy_env(self.env_overrides.clone()));
        env
    }

    /// The configured `Command`, not yet spawned — adjust stdio, working
    /// directory or anything else before you run it.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.executable);
        cmd.args(self.build_args());
        cmd.env_clear();
        for (k, v) in self.build_env() {
            cmd.env(OsString::from(k), OsString::from(v));
        }
        cmd
    }

    /// Spawn the browser with stdout/stderr inherited.
    pub fn spawn(&self) -> std::io::Result<Child> {
        self.command().spawn()
    }

    /// Spawn with stdout and stderr discarded.
    pub fn spawn_quiet(&self) -> std::io::Result<Child> {
        self.command()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }

    /// Spawn, then wait for the browser to answer on its debugging port and
    /// return its DevTools WebSocket URL.
    ///
    /// That URL is what every driver attaches to, and it is also the connection
    /// identity to key the registration lock on — so this is usually the last
    /// launch step you need:
    ///
    /// ```no_run
    /// # use chromeleon::launch::Launcher;
    /// # use std::time::Duration;
    /// let (child, ws_url) = Launcher::new("/opt/chromeleon/chrome")
    ///     .remote_debugging_port(9222)
    ///     .spawn_and_wait(Duration::from_secs(20))?;
    /// # let _ = (child, ws_url);
    /// # Ok::<(), std::io::Error>(())
    /// ```
    ///
    /// Requires [`Launcher::remote_debugging_port`]: there is nowhere to look
    /// otherwise. If the browser exits instead of listening, this returns the
    /// timeout rather than hanging — but the child is yours to reap either way.
    pub fn spawn_and_wait(&self, timeout: Duration) -> std::io::Result<(Child, String)> {
        let port = self.debugging_port.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "spawn_and_wait needs remote_debugging_port(...) — with no port there is \
                 nothing to wait for",
            )
        })?;
        let mut child = self.spawn_quiet()?;
        match devtools_url(port, timeout) {
            Ok(url) => Ok((child, url)),
            Err(e) => {
                // Do not leave a browser running that nobody has a handle to.
                let _ = child.kill();
                let _ = child.wait();
                Err(e)
            }
        }
    }
}

/// Poll `http://127.0.0.1:<port>/json/version` until the browser answers, and
/// return its `webSocketDebuggerUrl`.
///
/// A hand-written request rather than an HTTP dependency: one localhost GET is
/// not worth a TLS stack in every consumer's build.
pub fn devtools_url(port: u16, timeout: Duration) -> std::io::Result<String> {
    let deadline = Instant::now() + timeout;
    let mut last = String::from("never connected");
    while Instant::now() < deadline {
        match json_version(port) {
            Ok(body) => match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(parsed) => match parsed["webSocketDebuggerUrl"].as_str() {
                    Some(url) => return Ok(url.to_string()),
                    None => last = format!("no webSocketDebuggerUrl in {body}"),
                },
                Err(e) => last = format!("unparseable /json/version: {e}"),
            },
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("browser never exposed CDP on 127.0.0.1:{port} ({last})"),
    ))
}

fn json_version(port: u16) -> std::io::Result<String> {
    let stream = TcpStream::connect(("127.0.0.1", port))?;
    // Chrome's DevTools HTTP endpoint ignores `Connection: close` and holds the
    // socket open, so reading to EOF never returns. Two defences: a read
    // timeout, and reading exactly Content-Length bytes instead of to EOF.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut writer = &stream;
    write!(
        writer,
        "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut reader = BufReader::new(&stream);

    let mut line = String::new();
    let mut content_length: Option<usize> = None;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before the body",
            ));
        }
        if line.trim().is_empty() {
            break; // end of headers
        }
        if let Some(value) = line
            .split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim())
        {
            content_length = value.parse().ok();
        }
    }

    match content_length {
        Some(len) => {
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body)?;
            String::from_utf8(body)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }
        // No Content-Length: read until the peer stops talking or the timeout
        // trips, and keep whatever arrived.
        None => {
            let mut body = Vec::new();
            match reader.read_to_end(&mut body) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e),
            }
            if body.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "no JSON body in /json/version",
                ));
            }
            String::from_utf8(body)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caller_keeps_their_own_webrtc_policy() {
        let args = launch_args(["--webrtc-ip-handling-policy=default"], false, None);
        assert_eq!(args, ["--webrtc-ip-handling-policy=default"]);
    }

    #[test]
    fn captcha_flags_are_added_only_when_asked() {
        assert_eq!(launch_args::<[&str; 0]>([], false, None).len(), 1);
        let on = launch_args::<[&str; 0]>([], true, None);
        assert!(on.contains(&"--captcha-solver".to_string()));
        let with_models = launch_args::<[&str; 0]>([], false, Some("/models"));
        assert!(with_models.contains(&"--captcha-solver".to_string()));
        assert!(with_models.contains(&"--captcha-model-path=/models".to_string()));
    }

    #[test]
    fn proxy_variables_never_reach_the_browser() {
        let env = filter_proxy_env([
            ("HTTP_PROXY", "http://controller:3128"),
            ("https_proxy", "http://controller:3128"),
            ("PROXY_USER", "u"),
            ("PATH", "/usr/bin"),
            ("CHROMELEON_EXIT_IP", "1.2.3.4"),
        ]);
        assert_eq!(
            env.keys().cloned().collect::<Vec<_>>(),
            ["CHROMELEON_EXIT_IP", "PATH"]
        );
    }

    #[test]
    fn persona_flags_land_on_the_command_line() {
        let args = Launcher::new("/opt/chromeleon/chrome")
            .fingerprint_os("Linux")
            .fingerprint_seed(42)
            .build_args();
        assert!(args.contains(&"--fingerprint-os=Linux".to_string()));
        assert!(args.contains(&"--fingerprint-seed=42".to_string()));
    }

    #[test]
    fn page_settle_is_opt_in_and_never_a_default() {
        use crate::settle::SettleTuning;

        // It changes what the browser does, so it is not in LAUNCH_ARGS and not
        // in the args every launch gets.
        assert!(!LAUNCH_ARGS.contains(&"--page-settle"));
        assert!(!launch_args::<[&str; 0]>([], false, None)
            .iter()
            .any(|a| a.starts_with("--page-settle")));
        assert!(!Launcher::new("/x")
            .build_args()
            .iter()
            .any(|a| a.starts_with("--page-settle")));

        let tuned = Launcher::new("/x")
            .page_settle_tuning(
                SettleTuning::new()
                    .quiet_window_ms(1500)
                    .min_chars(64)
                    .timeout_ms(20_000)
                    .sample_interval_ms(100)
                    .pierce_shadow(true),
            )
            .build_args();
        for expected in [
            "--page-settle",
            "--page-settle-quiet-window-ms=1500",
            "--page-settle-min-chars=64",
            "--page-settle-timeout-ms=20000",
            "--page-settle-sample-interval-ms=100",
            "--page-settle-pierce-shadow",
        ] {
            assert!(
                tuned.contains(&expected.to_string()),
                "{expected} missing: {tuned:?}"
            );
        }

        // A tuning switch the caller set themselves is not duplicated or
        // overridden, exactly like the WebRTC policy.
        let by_hand = Launcher::new("/x")
            .arg("--page-settle-quiet-window-ms=250")
            .page_settle_tuning(SettleTuning::new().quiet_window_ms(1500))
            .build_args();
        assert!(by_hand.contains(&"--page-settle-quiet-window-ms=250".to_string()));
        assert_eq!(
            by_hand
                .iter()
                .filter(|a| a.starts_with("--page-settle-quiet-window-ms"))
                .count(),
            1
        );
        // …and page_settle(false) takes the whole thing back off.
        assert!(!Launcher::new("/x")
            .page_settle(true)
            .page_settle(false)
            .build_args()
            .iter()
            .any(|a| a.starts_with("--page-settle")));
    }

    #[test]
    fn suppressed_defaults_are_matched_by_switch_name() {
        assert!(is_suppressed_default_arg("--disable-extensions"));
        assert!(is_suppressed_default_arg("--metrics-recording-only=1"));
        assert!(!is_suppressed_default_arg("--headless=new"));
        let kept = without_suppressed_defaults(["--headless=new", "--disable-breakpad"]);
        assert_eq!(kept, ["--headless=new"]);
    }
}
