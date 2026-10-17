//! The form of an operation (DESIGN-09 G3), made from its input schema (`legionctl_api::form`): a text field for
//! words, numbers, paths and lists, yes and no for a flag, a button per word for a choice. The required fields first;
//! the optional ones on demand. Continue checks the input and goes on with the operation's own approval.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Selectable as _, Sizable as _, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use legionctl_api::OPS;
use legionctl_api::form::Kind;

use crate::LegionApp;
use crate::theme::{R_SM, S1, S2, S3, T_SM, T_XS, Tokens, card, heading};

impl LegionApp {
    /// A key in the form's focused field; false when the key is not the form's.
    pub(crate) fn form_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let k = &event.keystroke;
        if k.key == "enter" {
            self.submit_form(cx);
            return true;
        }
        let Some(form) = self.state.form.as_mut() else {
            return false;
        };
        let Some(field) = form.focused.and_then(|i| form.fields.get(i)) else {
            return false;
        };
        let text = form.values.entry(field.name.clone()).or_default();
        match (k.key.as_str(), &k.key_char) {
            ("backspace", _) => {
                text.pop();
            }
            ("tab", _) => {
                let next = form.focused.map_or(0, |i| i + 1);
                form.focused = (next < form.fields.len()).then_some(next);
            }
            (_, Some(c)) if !k.modifiers.control && !k.modifiers.alt => text.push_str(c),
            _ => return false,
        }
        cx.notify();
        true
    }

    /// A field's text: its value with the caret when it takes the keys, else its default.
    fn text_box(
        &self,
        i: usize,
        f: &legionctl_api::form::Field,
        value: &str,
        active: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = if value.is_empty() && !active {
            div().text_color(t.muted).child(if f.default.is_empty() {
                "–".to_string()
            } else {
                format!("default {}", f.default)
            })
        } else {
            div()
                .text_color(t.text)
                .child(format!("{value}{}", if active { "▌" } else { "" }))
        };
        div()
            .id(SharedString::from(format!("form-field-{}", f.name)))
            .track_focus(&self.form_focus)
            .min_w(px(220.))
            .flex_1()
            .px(S2)
            .py(px(4.))
            .rounded(R_SM)
            .border_1()
            .border_color(if active {
                t.accent.opacity(0.6)
            } else {
                t.border
            })
            .bg(t.window)
            .cursor_text()
            .text_size(T_SM)
            .child(shown)
            .on_click(cx.listener(move |app, _, window, cx| {
                if let Some(form) = app.state.form.as_mut() {
                    form.focused = Some(i);
                }
                app.form_focus.focus(window, cx);
                cx.notify();
            }))
            .test_support()
            .into_any_element()
    }

    /// The form card of the button's operation, above the tab's content.
    pub(crate) fn form(&self, t: &Tokens, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let form = self.state.form.as_ref()?;
        let op = OPS.iter().find(|o| o.name == form.button.op)?;
        let focused = self.form_focus.is_focused(window);
        let rows: Vec<AnyElement> = form
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.required || form.all)
            .map(|(i, f)| {
                let value = form.values.get(&f.name).cloned().unwrap_or_default();
                let control = match &f.kind {
                    Kind::Bool => choice(&f.name, &["yes", "no"], &value, cx),
                    Kind::Choice(words) => {
                        let words: Vec<&str> = words.iter().map(String::as_str).collect();
                        choice(&f.name, &words, &value, cx)
                    }
                    _ => self.text_box(i, f, &value, focused && form.focused == Some(i), t, cx),
                };
                div()
                    .v_flex()
                    .gap(px(2.))
                    .child(
                        div()
                            .h_flex()
                            .gap(S3)
                            .child(
                                div()
                                    .w(px(140.))
                                    .flex_none()
                                    .text_size(T_SM)
                                    .text_color(t.text)
                                    .child(format!(
                                        "{}{}",
                                        label(&f.name),
                                        if f.required { " *" } else { "" }
                                    )),
                            )
                            .child(control),
                    )
                    .child(
                        div()
                            .pl(px(152.))
                            .text_size(T_XS)
                            .text_color(t.muted)
                            .child(f.help.clone()),
                    )
                    .into_any_element()
            })
            .collect();
        let optional = form.fields.iter().filter(|f| !f.required).count();
        Some(
            card(t)
                .id("form")
                .bg(t.raised)
                .border_color(t.accent.opacity(0.5))
                .child(form_head(&form.button.label, about(form, op), t, cx))
                .children(rows)
                // the environment step shows all its fields
                .when(optional > 0 && form.button.op != "thread.repo", |c| {
                    c.child(
                        Button::new("form-more")
                            .ghost()
                            .small()
                            .label(if form.all {
                                "Fewer options".to_string()
                            } else {
                                format!("{optional} more options")
                            })
                            .on_click(cx.listener(|app, _, _, cx| {
                                if let Some(form) = app.state.form.as_mut() {
                                    form.all = !form.all;
                                }
                                cx.notify();
                            })),
                    )
                })
                .children(form.error.as_ref().map(|e| {
                    div()
                        .id("form-error")
                        .text_size(T_SM)
                        .text_color(t.critical)
                        .child(e.clone())
                        .test_support()
                }))
                .test_support(),
        )
        .map(|c| div().child(c))
    }
}

/// The form's head: the button's words, what the form is for, Continue and Cancel.
fn form_head(label: &str, about: &'static str, t: &Tokens, cx: &mut Context<LegionApp>) -> Div {
    div()
        .h_flex()
        .justify_between()
        .gap(S3)
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .gap(S1)
                .child(heading(label.to_string(), t))
                .child(div().text_size(T_SM).text_color(t.muted).child(about)),
        )
        .child(
            div()
                .h_flex()
                .flex_none()
                .gap(S2)
                .child(
                    Button::new("form-submit")
                        .primary()
                        .label("Continue")
                        .on_click(cx.listener(|app, _, _, cx| app.submit_form(cx))),
                )
                .child(
                    Button::new("form-cancel")
                        .label("Cancel")
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.state.form = None;
                            cx.notify();
                        })),
                ),
        )
}

/// A button per word; the chosen one is selected.
fn choice(name: &str, words: &[&str], value: &str, cx: &mut Context<LegionApp>) -> AnyElement {
    div()
        .h_flex()
        .flex_wrap()
        .gap(S1)
        .children(words.iter().map(|w| {
            let (field, word) = (name.to_string(), (*w).to_string());
            Button::new(SharedString::from(format!("form-{name}-{w}")))
                .small()
                .outline()
                .selected(value == *w)
                .label((*w).to_string())
                .on_click(cx.listener(move |app, _, _, cx| {
                    if let Some(form) = app.state.form.as_mut() {
                        form.values.insert(field.clone(), word.clone());
                    }
                    cx.notify();
                }))
        }))
        .into_any_element()
}

/// What the form is for: the operation's summary, or the environment step's own words.
fn about(form: &crate::Form, op: &legionctl_api::OpInfo) -> &'static str {
    match form.then {
        crate::Then::TabRepo => "Choose a git repository.",
        _ if op.name == "thread.repo" => "Choose where the agent works.",
        _ => op.summary(),
    }
}

/// A field's words for the user: `max_tokens` is "Max tokens".
pub(crate) fn label(name: &str) -> String {
    // a `repo` field is a folder in a repository, or a plain folder
    if name == "repo" {
        return "Folder".into();
    }
    let words = name.replace('_', " ");
    let mut chars = words.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

/// The given fields of `input` as (label, value) in the form's order: a flag as yes or no, a list with commas.
pub(crate) fn values(
    op: &legionctl_api::OpInfo,
    input: &serde_json::Value,
) -> Vec<(String, String)> {
    let texts = legionctl_api::form::texts(input);
    legionctl_api::form::fields(op)
        .into_iter()
        .filter(|f| input.get(&f.name).is_some_and(|v| !v.is_null()))
        .map(|f| {
            let value = match (&f.kind, texts[&f.name].as_str()) {
                (Kind::Bool, "true") => "yes".to_string(),
                (Kind::Bool, "false") => "no".to_string(),
                (_, v) => v.to_string(),
            };
            (label(&f.name), value)
        })
        .collect()
}
