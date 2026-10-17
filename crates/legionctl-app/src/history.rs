//! The chat bar's input history (user 2026-10-17): Up and Down recall the lines sent before, as a shell does. Each
//! line is a fact of the user (entity `chat:history`, attribute `line`), so the history is the same in each tab and
//! comes back after a restart.

use gpui_kit::Context;
use legionctl_api::{FactSetIn, FactsHistoryIn, op};

use crate::LegionApp;
use crate::shell::now_ms;

/// The facts entity of the history.
const ENTITY: &str = "chat:history";
/// The lines kept in memory: the newest.
const KEEP: usize = 200;

/// The lines sent (oldest first), the line that Up or Down shows, and the text typed before the first Up.
#[derive(Debug, Default)]
pub struct History {
    pub lines: Vec<String>,
    at: Option<usize>,
    draft: String,
}

impl LegionApp {
    /// Read the kept lines; lines sent before they come stay the newest.
    pub(crate) fn read_history(&mut self, cx: &mut Context<Self>) {
        let start = now_ms();
        let client = self.client.clone();
        let input = FactsHistoryIn {
            entity: ENTITY.into(),
        };
        let task = self.operator(
            cx,
            async move { client.call::<op::FactsHistory>(&input).await },
        );
        self.finish(cx, task, move |app, mut facts| {
            facts.sort_by_key(|f| f.valid_from);
            let sent = std::mem::take(&mut app.screen.history.lines);
            let lines = &mut app.screen.history.lines;
            lines.extend(
                facts
                    .into_iter()
                    // a newer line closes the valid time of the one before as a new row: the rows recorded now
                    .filter(|f| f.attribute == "line" && f.recorded_to.is_none())
                    // a line sent since the read started is in memory already
                    .filter(|f| f.valid_from < start)
                    .filter_map(|f| f.value.as_str().map(String::from))
                    .chain(sent),
            );
            let over = lines.len().saturating_sub(KEEP);
            lines.drain(..over);
        });
    }

    /// Keep a line that the chat bar sent: in memory now, and as a fact. A line equal to the last one is not kept again.
    pub(crate) fn keep_line(&mut self, line: &str, cx: &mut Context<Self>) {
        let history = &mut self.screen.history;
        history.at = None;
        if history.lines.last().map(String::as_str) == Some(line) {
            return;
        }
        history.lines.push(line.to_string());
        let client = self.client.clone();
        let input = FactSetIn {
            entity: ENTITY.into(),
            attribute: "line".into(),
            value: serde_json::Value::from(line).to_string(),
            scope: legionctl_api::FactScope::Global,
            repo: legionctl_api::ClientPath(self.screen.repo.clone()),
            dossier: None,
            thread: None,
            valid_from: Some(now_ms()),
            valid_to: None,
            from_ref: None,
            to_ref: None,
            anchor: None,
            evidence: Some("the chat bar's input".into()),
            confidence: 1.0,
            corrects: None,
        };
        let task = self.operator(cx, async move { client.call::<op::FactsSet>(&input).await });
        cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                let _ = this.update(cx, |app: &mut LegionApp, cx| {
                    app.state.message = Some(format!("Cannot save the chat line: {e:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Up (`back`) or Down in the chat bar: the line before or after in the history; past the newest, the text that
    /// was typed before the first Up.
    pub(crate) fn recall(&mut self, back: bool) {
        let n = self.screen.history.lines.len();
        if n == 0 {
            return;
        }
        let at = match (self.screen.history.at, back) {
            (None, true) => {
                self.screen.history.draft = self.screen.chat().to_string();
                Some(n - 1)
            }
            (None, false) => return,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i + 1 < n => Some(i + 1),
            (Some(_), false) => None,
        };
        self.screen.history.at = at;
        *self.screen.chat_mut() = match at {
            Some(i) => self.screen.history.lines[i].clone(),
            None => std::mem::take(&mut self.screen.history.draft),
        };
    }
}
