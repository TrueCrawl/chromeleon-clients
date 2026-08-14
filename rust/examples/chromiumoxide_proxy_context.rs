//! The handshake with chromiumoxide, end to end.
//!
//! ```sh
//! cargo run --features chromiumoxide --example chromiumoxide_proxy_context -- \
//!     /opt/chromeleon/chrome 'http://user:pass@gateway:12321'
//! ```
//!
//! Launching goes through [`Launcher`] rather than `Browser::launch`, because
//! chromiumoxide's `BrowserConfig` can only *add* environment variables — it has
//! no way to remove the controller's `PROXY_*`, which the handshake requires.
//! So: spawn with a clean environment, then attach.

use std::time::Duration;

use chromeleon::launch::Launcher;
use chromeleon::oxide::new_proxy_context;
use chromiumoxide::cdp::browser_protocol::target::CreateTargetParams;
use chromiumoxide::Browser;
use futures_lite::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let executable = args
        .next()
        .unwrap_or_else(|| "/opt/chromeleon/chrome".to_string());
    let proxy = args
        .next()
        .ok_or("usage: <chromeleon-binary> <proxy-url>")?;

    let (mut child, ws_url) = Launcher::new(&executable)
        .remote_debugging_port(9222)
        .arg("--headless=new")
        // Without this the persona is Windows, whatever the host is: the OS is
        // read from the command line when the context is created.
        .fingerprint_os("Linux")
        .spawn_and_wait(Duration::from_secs(30))?;
    println!("browser at {ws_url}");

    let (browser, mut handler) = Browser::connect(ws_url).await?;
    // chromiumoxide's handler is the connection's event pump; nothing works
    // until something drives it.
    let pump = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let context = new_proxy_context(&browser, proxy.as_str()).await?;
    println!("browserContextId: {}", context.inner());

    let mut target = CreateTargetParams::new("https://api.ipify.org?format=json");
    target.browser_context_id = Some(context);
    let page = browser.new_page(target).await?;
    page.wait_for_navigation().await?;
    let egress: String = page
        .evaluate("document.body.innerText")
        .await?
        .into_value()?;
    println!("egress through the proxy: {}", egress.trim());

    // A second context on a different proxy would run concurrently; a second on
    // the SAME proxy queues behind this one, which is the browser's rule, not a
    // choice this client makes.
    drop(page);
    pump.abort();
    let _ = child.kill();
    Ok(())
}
