//! A stand-in desktop notification service (`org.freedesktop.Notifications`) on a private bus, for the operator's
//! notices and the questions it asks the user.

/// The arguments of `org.freedesktop.Notifications.Notify`: app, replaces id, icon, summary, body, actions, hints,
/// timeout.
pub type Notify = (
    String,
    u32,
    String,
    String,
    String,
    Vec<String>,
    std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    i32,
);

/// A stand-in desktop on the private bus: it records the body of each notification and answers the n-th with
/// the n-th of `answers` (an action key) as `ActionInvoked`.
pub async fn answering(
    address: &str,
    answers: &'static [&'static str],
) -> (
    zbus::Connection,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let desktop = zbus::connection::Builder::address(address)
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .build()
        .await
        .unwrap();
    let seen = asked.clone();
    let bus = desktop.clone();
    tokio::spawn(async move {
        let mut calls = zbus::MessageStream::from(&bus);
        while let Some(Ok(call)) = futures_util::StreamExt::next(&mut calls).await {
            if call
                .header()
                .member()
                .is_some_and(|m| m.as_str() == "Notify")
            {
                let (_, _, _, _, body, actions, ..): Notify = call.body().deserialize().unwrap();
                // a question offers Allow and Deny; a notice offers nothing and gets no answer
                let question = !actions.is_empty();
                assert!(
                    !question || actions == ["allow", "Allow", "deny", "Deny"],
                    "{actions:?}"
                );
                let n = {
                    let mut seen = seen.lock().unwrap();
                    seen.push(body);
                    seen.len()
                };
                let id = u32::try_from(n).unwrap();
                bus.reply(&call.header(), &(id,)).await.unwrap();
                if question {
                    bus.emit_signal(
                        None::<()>,
                        "/org/freedesktop/Notifications",
                        "org.freedesktop.Notifications",
                        "ActionInvoked",
                        &(id, answers[n - 1]),
                    )
                    .await
                    .unwrap();
                }
            }
        }
    });
    (desktop, asked)
}
