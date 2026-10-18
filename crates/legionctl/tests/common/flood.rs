//! The daemon's session events as the operator's recorder writes them, for the tests of writers under load.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use legionctl_core::db::Db;
use serde_json::json;

/// `session.open` events at 1000 a second (20 times the peak of r361) into the operator database `db` until `done`,
/// written as the recorder writes them: all that came in during the last write in one transaction, through the event
/// log's connection (which does not sync each commit). Returns the number of events.
pub fn recorder_flood(db: &Path, done: &AtomicBool) -> usize {
    let start = Instant::now();
    let mut n = 0;
    while !done.load(Ordering::Relaxed) {
        let due = usize::try_from(start.elapsed().as_millis()).unwrap();
        if due <= n {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let batch: Vec<_> = (n..due)
            .map(|i| {
                (
                    "session.open".to_string(),
                    json!({"session": "x", "path": i}),
                )
            })
            .collect();
        Db::open_log(db)
            .and_then(|db| db.record_all("legionctl-system", &batch))
            .unwrap();
        n = due;
    }
    n
}
