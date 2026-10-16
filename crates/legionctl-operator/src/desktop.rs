//! The desktop: notices for the user, and the question an agent's call outside its own session waits for (DESIGN/03),
//! a notification with Allow and Deny; no answer within the time, a closed notification, or no desktop to ask is a
//! no.

use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const WAIT: Duration = Duration::from_secs(120);

/// Show the user a notice that stays until closed (urgency critical).
///
/// # Errors
/// There is no notification service on `bus`.
pub async fn tell(bus: &zbus::Connection, icon: &str, summary: &str, body: &str) -> Result<()> {
    let hints = std::collections::HashMap::from([("urgency", zbus::zvariant::Value::U8(2))]);
    // Notify: app, replaces id, icon, summary, body, actions, hints, timeout
    let args = (
        "legionctl",
        0u32,
        icon,
        summary,
        body,
        Vec::<&str>::new(),
        hints,
        0i32,
    );
    // a desktop that does not answer must not hold up the caller
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        bus.call_method(
            Some(NOTIFICATIONS),
            "/org/freedesktop/Notifications",
            Some(NOTIFICATIONS),
            "Notify",
            &args,
        ),
    )
    .await
    .context("the desktop notification service does not answer")?
    .context("no desktop notification service")?;
    Ok(())
}

/// Ask the user, with the title `summary`, whether `what` may go ahead; true only for Allow.
///
/// # Errors
/// There is no session bus or notification service to ask.
pub async fn ask(summary: &str, what: &str) -> Result<bool> {
    Ok(question(summary, what).await? == Some(true))
}

/// Ask the user `body` with the title `summary`, with Allow and Deny: the answer, or `None` when the user closed the
/// notification or gave no answer within the time.
///
/// # Errors
/// There is no session bus or notification service to ask.
pub async fn question(summary: &str, body: &str) -> Result<Option<bool>> {
    let bus = zbus::Connection::session()
        .await
        .context("no session bus to ask the user")?;
    // listen before the notification is shown, so a fast answer is not lost
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(NOTIFICATIONS)?
        .build();
    let mut answers = zbus::MessageStream::for_match_rule(rule, &bus, None).await?;
    let hints = std::collections::HashMap::from([("urgency", zbus::zvariant::Value::U8(2))]);
    // Notify: app, replaces id, icon, summary, body, actions (key, label pairs), hints, timeout (0: until closed)
    let args = (
        "legionctl",
        0u32,
        "dialog-question",
        summary,
        body,
        vec!["allow", "Allow", "deny", "Deny"],
        hints,
        0i32,
    );
    let reply = bus
        .call_method(
            Some(NOTIFICATIONS),
            "/org/freedesktop/Notifications",
            Some(NOTIFICATIONS),
            "Notify",
            &args,
        )
        .await
        .context("no desktop notification service to ask the user")?;
    let id: u32 = reply.body().deserialize()?;
    let answer = tokio::time::timeout(WAIT, async {
        while let Some(signal) = answers.next().await {
            let signal = signal.context("reading the notification service's answer")?;
            match signal.header().member().map(ToString::to_string).as_deref() {
                Some("ActionInvoked") => {
                    let (of, key): (u32, String) = signal.body().deserialize()?;
                    if of == id {
                        return Ok(Some(key == "allow"));
                    }
                }
                Some("NotificationClosed") => {
                    let (of, _reason): (u32, u32) = signal.body().deserialize()?;
                    if of == id {
                        return Ok(None);
                    }
                }
                _ => {}
            }
        }
        anyhow::bail!("the notification service's signals ended before an answer")
    })
    .await;
    if answer.is_err() {
        let _ = bus
            .call_method(
                Some(NOTIFICATIONS),
                "/org/freedesktop/Notifications",
                Some(NOTIFICATIONS),
                "CloseNotification",
                &(id,),
            )
            .await;
    }
    // no answer within the time is no decision (None): the caller says so
    answer.unwrap_or(Ok(None))
}

const PORTAL: &str = "org.freedesktop.portal.Desktop";

/// A screenshot through the desktop portal (`org.freedesktop.portal.Screenshot`, interactive: the user picks the
/// window in the desktop's dialog): the PNG's bytes. The portal's file is removed once read.
///
/// # Errors
/// There is no session bus or portal, the user cancels, or no answer comes within the time.
pub async fn screenshot() -> Result<Vec<u8>> {
    let bus = zbus::Connection::session()
        .await
        .context("no session bus for the screenshot")?;
    let token = format!(
        "legionctl{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let sender = bus
        .unique_name()
        .context("the session bus gave no name")?
        .trim_start_matches(':')
        .replace('.', "_");
    let handle = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    // listen before the call, so a fast answer is not lost
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.portal.Request")?
        .member("Response")?
        .path(handle.as_str())?
        .build();
    let mut answers = zbus::MessageStream::for_match_rule(rule, &bus, None).await?;
    let options = std::collections::HashMap::from([
        ("handle_token", zbus::zvariant::Value::from(token.as_str())),
        ("interactive", zbus::zvariant::Value::from(true)),
    ]);
    bus.call_method(
        Some(PORTAL),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.Screenshot"),
        "Screenshot",
        &("", options),
    )
    .await
    .context("no desktop screenshot portal")?;
    let signal = tokio::time::timeout(WAIT, answers.next())
        .await
        .context("the screenshot portal gave no answer in time")?
        .context("the screenshot portal's signals ended before an answer")??;
    let (code, results): (
        u32,
        std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    ) = signal.body().deserialize()?;
    anyhow::ensure!(
        code == 0,
        "the screenshot was cancelled (portal answer {code})"
    );
    let uri: String = results
        .get("uri")
        .context("the screenshot portal's answer has no uri")?
        .try_clone()?
        .try_into()?;
    let path = file_path(&uri)?;
    let png = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(png)
}

/// The path of a `file://` URI, with its percent escapes decoded.
fn file_path(uri: &str) -> Result<std::path::PathBuf> {
    let rest = uri
        .strip_prefix("file://")
        .with_context(|| format!("{uri} is not a file URI"))?;
    let mut bytes = Vec::with_capacity(rest.len());
    let mut it = rest.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hex = [it.next(), it.next()];
            let hex: Option<Vec<u8>> = hex.into_iter().collect();
            let byte = hex
                .and_then(|h| u8::from_str_radix(std::str::from_utf8(&h).ok()?, 16).ok())
                .with_context(|| format!("{uri} has a bad escape"))?;
            bytes.push(byte);
        } else {
            bytes.push(b);
        }
    }
    Ok(<std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(bytes).into())
}
