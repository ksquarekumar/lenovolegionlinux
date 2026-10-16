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
