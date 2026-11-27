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
        "⚠ llmrace alert: {} {} degraded {:.0} → {:.0} {} (+{:.0}%) at round {}",
        alert.provider,
        alert.metric,
        alert.baseline,
        alert.current,
        if alert.metric == "availability" {
            "% failed rounds"
        } else {
            "ms"
        },
        alert.delta_pct,
        alert.round
    );
    let resp = client
        .post(url)
        .json(&serde_json::json!({ "text": text, "content": text }))
        .send()
        .await
        .map_err(|e| Error::Stream(format!("alert webhook: {}", e.without_url())))?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = crate::util::read_body_capped(resp, 16 * 1024)
            .await
            .unwrap_or_default();
        return Err(Error::Http {
            provider: "webhook".into(),
            status,
            body: crate::util::truncate(&body, 200),
            retry_after: None,
        });
    }
    Ok(())
}

const MACOS_NOTIFICATION_SCRIPT: &str =
    "on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run";

const WINDOWS_NOTIFICATION_SCRIPT: &str = "\
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; \
[xml]$t = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02).GetXml(); \
$nodes = $t.GetElementsByTagName('text'); \
$nodes.Item(0).AppendChild($t.CreateTextNode($env:LLMRACE_TITLE)) | Out-Null; \
$nodes.Item(1).AppendChild($t.CreateTextNode($env:LLMRACE_BODY)) | Out-Null; \
$xml = New-Object Windows.Data.Xml.Dom.XmlDocument; \
$xml.LoadXml($t.OuterXml); \
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('llmrace').Show([Windows.UI.Notifications.ToastNotification]::new($xml))";

#[derive(Debug, PartialEq, Eq)]
struct NotificationCommand {
    program: &'static str,
    args: Vec<String>,
    envs: Vec<(&'static str, String)>,
}

#[allow(dead_code)] // exercised on all platforms by unit tests
fn macos_command(title: &str, body: &str) -> NotificationCommand {
    NotificationCommand {
        program: "osascript",
        args: vec![
            "-e".into(),
            MACOS_NOTIFICATION_SCRIPT.into(),
            "--".into(),
            title.into(),
            body.into(),
        ],
        envs: Vec::new(),
    }
}

#[allow(dead_code)] // exercised on all platforms by unit tests
fn linux_command(title: &str, body: &str) -> NotificationCommand {
    NotificationCommand {
        program: "notify-send",
        args: vec!["--".into(), title.into(), body.into()],
        envs: Vec::new(),
    }
}

#[allow(dead_code)] // exercised on all platforms by unit tests
fn windows_command(title: &str, body: &str) -> NotificationCommand {
    NotificationCommand {
        program: "powershell",
        args: vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            WINDOWS_NOTIFICATION_SCRIPT.into(),
        ],
        envs: vec![
            ("LLMRACE_TITLE", title.into()),
            ("LLMRACE_BODY", body.into()),
        ],
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn spawn_notification(spec: NotificationCommand) {
    let mut cmd = std::process::Command::new(spec.program);
    cmd.args(&spec.args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (k, v) in spec.envs {
        cmd.env(k, v);
    }
    let _ = cmd.spawn();
}

/// Sends a desktop notification (macOS `osascript`, Linux `notify-send`,
/// Windows toast via PowerShell). No-op on unsupported platforms.
pub fn desktop_notification(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    spawn_notification(macos_command(title, body));
    #[cfg(target_os = "linux")]
    spawn_notification(linux_command(title, body));
    #[cfg(target_os = "windows")]
    spawn_notification(windows_command(title, body));
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let _ = (title, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_notification_passes_untrusted_text_via_argv_not_script() {
        let title = "-u \"$(id)\" \\ 'evil'";
        let body = "line1\n\"do shell script \\\"pwn\\\"\"";
        let cmd = macos_command(title, body);
        assert_eq!(cmd.program, "osascript");
        assert_eq!(cmd.args.len(), 5);
        assert_eq!(cmd.args[0], "-e");
        assert_eq!(cmd.args[1], MACOS_NOTIFICATION_SCRIPT);
        assert!(!cmd.args[1].contains("evil"));
        assert!(!cmd.args[1].contains("pwn"));
        assert_eq!(cmd.args[2], "--");
        assert_eq!(cmd.args[3], title);
        assert_eq!(cmd.args[4], body);
    }

    #[test]
    fn linux_notification_terminates_options_before_leading_dash_inputs() {
        let title = "--urgency=critical";
        let body = "-a \"fake\" \\ 'quoted'";
        let cmd = linux_command(title, body);
        assert_eq!(cmd.program, "notify-send");
        assert_eq!(cmd.args, vec!["--", title, body]);
    }

    #[test]
    fn windows_notification_uses_static_script_and_env_vars() {
        let title = "'; Start-Process calc; '";
        let body = "\"$(whoami)\" \\ -Command";
        let cmd = windows_command(title, body);
        assert_eq!(cmd.program, "powershell");
        assert_eq!(
            cmd.args,
            vec![
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                WINDOWS_NOTIFICATION_SCRIPT
            ]
        );
        assert!(WINDOWS_NOTIFICATION_SCRIPT.contains("[xml]$t"));
        assert!(!WINDOWS_NOTIFICATION_SCRIPT.contains("$[xml]$t"));
        assert!(!WINDOWS_NOTIFICATION_SCRIPT.contains("Start-Process"));
        assert!(!WINDOWS_NOTIFICATION_SCRIPT.contains("whoami"));
        assert_eq!(
            cmd.envs,
            vec![
                ("LLMRACE_TITLE", title.to_string()),
                ("LLMRACE_BODY", body.to_string()),
            ]
        );
    }
}
