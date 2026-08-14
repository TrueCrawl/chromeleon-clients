//! Spawn Chromeleon with CDP exposed and print the WebSocket URL to attach to.
//!
//! ```sh
//! cargo run --example launch_and_attach -- /opt/chromeleon/chrome
//! ```
//!
//! This is the launch-once-attach-many shape: the browser is a process you own,
//! and every driver (chromiumoxide, headless_chrome, a raw socket) attaches to
//! the URL printed here. [`Launcher`] contributes the two things a driver cannot
//! know — the WebRTC handling policy a per-context proxy needs, and a browser
//! environment with the controller's `PROXY_*` variables removed.

use std::time::Duration;

use chromeleon::launch::{devtools_url, Launcher, LAUNCH_ARGS};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/opt/chromeleon/chrome".to_string());
    let port: u16 = std::env::args()
        .nth(2)
        .map(|p| p.parse())
        .transpose()?
        .unwrap_or(9222);

    let launcher = Launcher::new(&executable)
        .remote_debugging_port(port)
        .arg("--headless=new")
        .arg("--no-first-run");

    println!("args:");
    for arg in launcher.build_args() {
        let ours = LAUNCH_ARGS.contains(&arg.as_str());
        println!("  {arg}{}", if ours { "   <- added for you" } else { "" });
    }
    let env = launcher.build_env();
    println!(
        "env: {} variables, none of them proxy-related (PROXY_* is the controller's, not the browser's)",
        env.len()
    );

    let mut child = launcher.spawn_quiet()?;
    println!("spawned pid {}", child.id());

    match devtools_url(port, Duration::from_secs(20)) {
        Ok(ws_url) => {
            println!("\nattach to: {ws_url}");
            println!("that URL is also the connection identity — pass it as the ConnKey.");
        }
        Err(e) => {
            let _ = child.kill();
            return Err(e.into());
        }
    }

    println!("\nctrl-c to stop, or kill pid {}", child.id());
    child.wait()?;
    Ok(())
}
