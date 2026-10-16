//! A stand-in desktop screenshot portal (`org.freedesktop.portal.Screenshot`) on a private bus: each call answers
//! with a copy of the same picture, in a file whose name needs escapes in its URI, and records whether it asked the
//! user (`interactive`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use zbus::zvariant::{ObjectPath, OwnedValue, Value};

/// The portal on `address`; its pictures go in `dir`. Returns its connection, the `interactive` option of each call
/// and the files it gave.
pub async fn screenshots(
    address: &str,
    dir: &Path,
    png: &'static [u8],
) -> (zbus::Connection, Arc<Mutex<Vec<(bool, PathBuf)>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let portal = zbus::connection::Builder::address(address)
        .unwrap()
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .build()
        .await
        .unwrap();
    let (bus, seen, dir) = (portal.clone(), asked.clone(), dir.to_path_buf());
    tokio::spawn(async move {
        let mut calls = zbus::MessageStream::from(&bus);
        while let Some(Ok(call)) = futures_util::StreamExt::next(&mut calls).await {
            let header = call.header();
            if header.member().is_none_or(|m| m.as_str() != "Screenshot") {
                continue;
            }
            let (_, options): (String, HashMap<String, OwnedValue>) =
                call.body().deserialize().unwrap();
            let token: String = options["handle_token"]
                .try_clone()
                .unwrap()
                .try_into()
                .unwrap();
            let interactive: bool = options["interactive"]
                .try_clone()
                .unwrap()
                .try_into()
                .unwrap();
            let sender = header
                .sender()
                .unwrap()
                .trim_start_matches(':')
                .replace('.', "_");
            let handle = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
            let n = seen.lock().unwrap().len();
            let file = dir.join(format!("Screenshot {n}.png"));
            std::fs::write(&file, png).unwrap();
            seen.lock().unwrap().push((interactive, file.clone()));
            bus.reply(&header, &(ObjectPath::try_from(handle.as_str()).unwrap(),))
                .await
                .unwrap();
            let uri = format!("file://{}", file.display().to_string().replace(' ', "%20"));
            let results = HashMap::from([("uri", Value::from(uri))]);
            bus.emit_signal(
                None::<()>,
                handle.as_str(),
                "org.freedesktop.portal.Request",
                "Response",
                &(0u32, results),
            )
            .await
            .unwrap();
        }
    });
    (portal, asked)
}
