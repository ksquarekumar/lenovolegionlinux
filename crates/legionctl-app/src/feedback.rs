//! Feedback (todo-168): the user's note with what the app shows (the tab's view and data, errors; the operator adds its last events)
//! and a screenshot of the window becomes an immutable dossier entry `feedbacks/<NNNN>-<slug>/` that agents read.
//! The dossier is the chat bar thread's, else the user's; with neither, the app asks for one and keeps the note.

use gpui_kit::{Context, Window};
use legionctl_api::{FeedbackIn, op};
use serde_json::json;

use crate::LegionApp;

impl LegionApp {
    /// The title bar's Feedback: the chat bar takes the note.
    pub(crate) fn ask_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        *self.screen.chat_mut() = "/feedback ".into();
        self.chat_focus.focus(window, cx);
        cx.notify();
    }

    /// `/feedback <note>`: keep the note, the state and a screenshot (the desktop asks which window).
    pub(crate) fn feedback(&mut self, note: &str, cx: &mut Context<Self>) {
        if note.is_empty() {
            self.state.message = Some("Type /feedback and your note".into());
            return;
        }
        let Some(dossier) = self
            .screen
            .thread
            .as_ref()
            .map(|t| t.thread.dossier.clone())
            .or_else(|| self.screen.dossier.clone())
        else {
            self.no_dossier(cx);
            *self.screen.chat_mut() = format!("/feedback {note}");
            self.state.message = Some(
                "Choose a dossier for your feedback: click Use on a dossier, or New dossier. Then press Enter: your \
                 note stays in the chat bar."
                    .into(),
            );
            return;
        };
        let key = self.tab_key();
        let view = self.screen.view();
        let mut about = std::collections::BTreeMap::from([
            (
                "app".to_string(),
                format!(
                    "{} api {}",
                    env!("CARGO_PKG_VERSION"),
                    legionctl_api::api_fingerprint()
                ),
            ),
            ("space".to_string(), format!("{:?}", self.screen.space)),
            ("tab".to_string(), key.clone()),
        ]);
        if let Some(v) = view {
            about.insert("view".into(), format!("{} version {}", v.name, v.version));
        }
        let state = json!({
            "tab": self.tab(),
            "view": view,
            "message": self.state.message,
            "offline": self.state.offline,
            "thread": self.screen.thread.as_ref().map(|t| t.thread.id),
        });
        let input = FeedbackIn {
            note: note.to_string(),
            dossier,
            about,
            state: Some(state.to_string()),
            screenshot: true,
        };
        let client = self.client.clone();
        let task = self.operator(
            cx,
            async move { client.call::<op::FeedbackNew>(&input).await },
        );
        self.state.message = Some("Feedback: choose the window in the screenshot dialog".into());
        self.finish(cx, task, |app, lines| {
            app.state.message = Some(lines.lines.join("\n"));
        });
    }
}
