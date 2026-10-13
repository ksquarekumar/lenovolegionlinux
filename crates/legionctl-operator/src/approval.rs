//! An agent's call outside its own session waits for the user (DESIGN/03): a desktop notification with Allow and
//! Deny. No answer within the time, a closed notification, or no desktop to ask is a no.

use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const WAIT: Duration = Duration::from_secs(120);

/// Ask the user whether `session` may do `what`; true only for Allow.
///
/// # Errors
/// There is no session bus or notification service to ask.
pub async fn ask(session: &str, what: &str) -> Result<bool> {
    let bus = zbus::Connection::session()
        .await
        .context("no session bus to ask the user")?;
    // listen before the notification is shown, so a fast answer is not lost
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(NOTIFICATIONS)?
        .build();
    let mut answers = zbus::MessageStream::for_match_rule(rule, &bus, None).await?;
    let short = &session[..session.len().min(8)];
    let hints = std::collections::HashMap::from([("urgency", zbus::zvariant::Value::U8(2))]);
    // Notify: app, replaces id, icon, summary, body, actions (key, label pairs), hints, timeout (0: until closed)
    let args = (
        "legionctl",
        0u32,
        "dialog-question",
        format!("legionctl: agent session {short} asks"),
        what,
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
        while let Some(Ok(signal)) = answers.next().await {
            match signal.header().member().map(ToString::to_string).as_deref() {
                Some("ActionInvoked") => {
                    let (of, key): (u32, String) = signal.body().deserialize()?;
                    if of == id {
                        return Ok(key == "allow");
                    }
                }
                Some("NotificationClosed") => {
                    let (of, _reason): (u32, u32) = signal.body().deserialize()?;
                    if of == id {
                        return Ok(false);
                    }
                }
                _ => {}
            }
        }
        anyhow::Ok(false)
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
    answer.unwrap_or(Ok(false))
}
