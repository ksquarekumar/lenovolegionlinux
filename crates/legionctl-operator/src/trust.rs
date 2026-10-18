//! Trust on first use of a repository's config layer (O-2, user decision 2026-10-16): a `.legionctl` file that sets
//! a powerful or machine setting (with `config.overrides`) is used for these keys only after the user allowed
//! its content once, through the same desktop question as an agent's call outside its session. The config resolves
//! without them meanwhile; the answer is stored with the content's hash, so a changed file asks again.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use legionctl_core::config::{Approvals, Unapproved};

static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();
/// `<file>\n<hash>` of each question on the desktop now: one question per content at a time.
static ASKING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// The runtime that asks; set once at the operator's start.
pub fn init(runtime: tokio::runtime::Handle) {
    let _ = RUNTIME.set(runtime);
    legionctl_core::config::set_asker(ask);
}

/// Ask the user about each file of `unapproved` that is not asked about already, in the background. An answer is
/// stored; no answer (closed, time over) stores nothing, so the next use asks again.
pub fn ask(unapproved: &[Unapproved]) {
    let Some(runtime) = RUNTIME.get() else {
        return;
    };
    for file in unapproved {
        let key = format!("{}\n{}", file.file, file.hash);
        let new = ASKING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(HashSet::new)
            .insert(key.clone());
        if !new {
            continue;
        }
        let file = file.clone();
        runtime.spawn(async move {
            let body = format!(
                "The repository file {} sets: {}. Allow this content? If you deny, legionctl does not use it \
                 until the file changes.",
                file.file,
                file.keys.join(", ")
            );
            let stored = match crate::desktop::question("legionctl: a repository's config asks", &body).await
            {
                Ok(Some(allowed)) => Approvals::answer(&file.file, &file.hash, allowed),
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            };
            if let Err(e) = stored {
                eprintln!("operator: the approval of {} is not stored: {e:#}", file.file);
            }
            ASKING
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_or_insert_with(HashSet::new)
                .remove(&key);
        });
    }
}
