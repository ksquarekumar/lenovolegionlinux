//! A normal journey's text is for the user (EV-review-12): no error, no stated limit of legionctl's own work and no
//! builder text. A test of a normal journey calls [`assert_plain`] on what the user reads.

/// The words that a normal journey never shows: an error or a raw OS error, a stated limit of legionctl's own work,
/// a missing value said as "no value", and builder text (plan and design ids, config keys and protocol names as the
/// fix).
const NOT_PLAIN: [&str; 17] = [
    "error",
    "no value",
    "os error",
    "panicked",
    "cannot be bound",
    "has no way",
    "not supported",
    "not built",
    "is not on",
    "todo-",
    "DESIGN-",
    "docs/design",
    "shortcut",
    "JSON-RPC",
    "(config ",
    "session.acp",
    "the client must",
];

/// Panic when `text` (what the user reads of `what`) has a word of [`NOT_PLAIN`].
pub fn assert_plain(what: &str, text: &str) {
    let lower = text.to_lowercase();
    let found: Vec<&str> = NOT_PLAIN
        .iter()
        .copied()
        .filter(|word| lower.contains(&word.to_lowercase()))
        .collect();
    assert!(
        found.is_empty(),
        "{what} is not plain user text ({found:?}):\n{text}"
    );
}
