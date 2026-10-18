//! The form of an operation (DESIGN-09 G3), made from its input schema (`legionctl_api::form`): a text field for
//! words, numbers, paths and lists, yes and no for a flag, a button per word for a choice. The required fields first;
//! the optional ones on demand. Continue checks the input and goes on with the operation's own approval.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Enter, Input};
use gpui_kit::component::{Selectable as _, Sizable as _, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, Styled as _, div, px,
};
use legionctl_api::OPS;
use legionctl_api::form::Kind;

use crate::LegionApp;
use crate::theme::{S1, S2, S3, T_SM, T_XS, Tokens, card, heading};

impl LegionApp {
    /// A field's text field; the middle button pastes the primary selection.
    fn text_box(&self, f: &legionctl_api::form::Field) -> Option<AnyElement> {
        let input = self.state.form.as_ref()?.inputs.get(&f.name)?;
        Some(
            div()
                .id(SharedString::from(format!("form-field-{}", f.name)))
                .min_w(px(220.))
                .flex_1()
                .on_mouse_down(MouseButton::Middle, crate::input::paste_primary(input))
                .child(Input::new(input).small().aria_label(label(&f.name)))
                .test_support()
                .into_any_element(),
        )
    }

    /// The form card of the button's operation, above the tab's content.
    pub(crate) fn form(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<Div> {
        let form = self.state.form.as_ref()?;
        let op = OPS.iter().find(|o| o.name == form.button.op)?;
        let rows: Vec<AnyElement> = form
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.required || form.all)
            .map(|(_, f)| {
                let value = form.values.get(&f.name).cloned().unwrap_or_default();
                let control = match &f.kind {
                    Kind::Bool => choice(&f.name, &["yes", "no"], &value, cx),
                    Kind::Choice(_) if form.selects.contains_key(&f.name) => {
                        dropdown(&f.name, &form.selects[&f.name].0)
                    }
                    Kind::Choice(words) => {
                        let words: Vec<&str> = words.iter().map(String::as_str).collect();
                        choice(&f.name, &words, &value, cx)
                    }
                    Kind::Choices(words) => several(&f.name, words, &value, cx),
                    // the render makes the text field before this frame's elements
                    _ => self.text_box(f).unwrap_or_else(|| div().into_any_element()),
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
                // Enter in a text field goes on, as Continue does
                .on_action(cx.listener(|app, _: &Enter, _, cx| app.submit_form(cx)))
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
                        .on_click(cx.listener(|app, _, _, cx| app.cancel_form(cx))),
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

/// The dropdown state of a choice with more words than buttons.
pub type Dropdown =
    gpui_kit::component::select::SelectState<gpui_kit::component::select::SearchableVec<String>>;

/// A dropdown of the words; its choice goes into the form.
fn dropdown(name: &str, state: &gpui_kit::Entity<Dropdown>) -> AnyElement {
    gpui_kit::component::select::Select::new(state)
        .id(SharedString::from(format!("form-{name}")))
        .small()
        .min_w(px(220.))
        .accessibility_label(label(name))
        .into_any_element()
}

/// A check box per word: any of them (the words are a list, with commas in the form's text).
fn several(name: &str, words: &[String], value: &str, cx: &mut Context<LegionApp>) -> AnyElement {
    let chosen: Vec<&str> = value
        .split(',')
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .collect();
    let app = cx.entity().downgrade();
    div()
        .h_flex()
        .flex_wrap()
        .gap(S2)
        .children(words.iter().map(|w| {
            let (field, word, app) = (name.to_string(), w.clone(), app.clone());
            gpui_kit::component::checkbox::Checkbox::new(SharedString::from(format!(
                "form-{name}-{w}"
            )))
            .label(w.clone())
            .checked(chosen.contains(&w.as_str()))
            .on_click(move |on, _, cx| {
                let _ = app.update(cx, |app, cx| {
                    let Some(form) = app.state.form.as_mut() else {
                        return;
                    };
                    let text = form.values.get(&field).cloned().unwrap_or_default();
                    let mut words: Vec<String> = text
                        .split(',')
                        .map(|w| w.trim().to_string())
                        .filter(|w| !w.is_empty() && *w != word)
                        .collect();
                    if *on {
                        words.push(word.clone());
                    }
                    form.values.insert(field.clone(), words.join(", "));
                    cx.notify();
                });
            })
        }))
        .into_any_element()
}

/// What the form is for: the operation's summary, or the environment step's own words.
fn about(form: &crate::Form, op: &legionctl_api::OpInfo) -> &'static str {
    match form.then {
        crate::Then::TabProject => "Choose a git repository.",
        _ if op.name == "thread.repo" => "Choose where the agent works.",
        _ => op.summary(),
    }
}

/// A field's words for the user: `max_tokens` is "Max tokens".
pub(crate) fn label(name: &str) -> String {
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
