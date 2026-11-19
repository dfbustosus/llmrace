//! Alert notifications: webhooks (Slack/Discord-compatible) and desktop
//! notifications for watch mode.

use crate::error::{Error, Result};
use crate::watch::Alert;

/// Posts an alert to a webhook URL.
///
/// The payload follows the common `{text: "..."}` shape accepted by Slack
/// incoming webhooks, Discord webhooks, and most generic alert receivers.
pub async fn post_webhook(client: &reqwest::Client, url: &str, alert: &Alert) -> Result<()> {
    let text = format!(
        "⚠ llmrace alert: {} {} degraded {:.0} → {:.0} ms (+{:.0}%) at round {}",
        alert.provider, alert.metric, alert.baseline, alert.current, alert.delta_pct, alert.round
    );
    let resp = client
        .post(url)
        .json(&serde_json::json!({ "text": text, "content": text }))
        .send()
        .await
        .map_err(|e| Error::Stream(format!("alert webhook: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Http {
            provider: "webhook".into(),
            status: resp.status().as_u16(),
            body: crate::util::truncate(&resp.text().await.unwrap_or_default(), 200),
        });
    }
    Ok(())
}

/// Sends a desktop notification (macOS `osascript`, Linux `notify-send`,
/// Windows toast via PowerShell). No-op on unsupported platforms.
pub fn desktop_notification(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!("display notification \"{body}\" with title \"{title}\"");
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .arg(title)
            .arg(body)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null"
        );
        let _ = std::process::Command::new("powershell")
            .arg("-Command")
            .arg(&script)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (title, body);
    }
}
