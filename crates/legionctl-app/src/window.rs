//! The window: the title bar with the operator's status, the panes (`shell`), the plan of a pending action above the
//! tab's content, and the last result below it. The kit's icons and the app's extra icons.

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Sizable as _, StyledExt as _, TitleBar};
use gpui_kit::{
    AnyElement, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, px,
};
use legionctl_api::Class;

use crate::LegionApp;
use crate::theme::{R_SM, S1, S2, S3, S5, T_SM, Tokens, card, heading, icon};
use crate::widgets::text;

gpui_kit::assets::icon_assets!(
    Extra,
    [
        Activity,
        Monitor,
        HeartPulse,
        ChartLine,
        Boxes,
        Fan,
        Thermometer,
        Zap,
        Gauge,
        Play,
        Wrench,
        MessageSquare,
        Users,
        FolderGit2,
        Pin,
        LayoutDashboard,
        Plus
    ]
);

/// The kit's icons and the extra icons of the app.
pub struct AppAssets;

impl gpui_kit::AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        match Extra.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(Extra.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

impl LegionApp {
    /// The pending action in the user's words (DESIGN-09: a session or workspace change confirms inline with its plan;
    /// a machine change shows its plan, then polkit asks): what the operation does, what its class changes, then the
    /// dry run's plan, or the input as labelled values; the operation and its raw input only behind Details.
    fn pending(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<Div> {
        let pending = self.state.pending.as_ref()?;
        let op = legionctl_api::OPS
            .iter()
            .find(|o| o.name == pending.button.op)?;
        let what = match pending.class {
            Class::Machine => "Changes the machine. Asks for your password.",
            Class::Workspace => "Changes your files or settings.",
            Class::Session => "Changes a setting of yours.",
            Class::Read => "Reads only.",
        };
        let empty = pending.plan.as_deref().is_some_and(|p| p.trim().is_empty());
        let lines = Self::pending_lines(pending, op, empty, t);
        // the buttons first: a long plan must not push them out of reach
        let buttons = Self::pending_buttons(empty, cx);
        let details = pending.details.then(|| {
            let input = serde_json::to_string_pretty(&pending.button.input).unwrap_or_default();
            div()
                .id("pending-raw")
                .px(S3)
                .py(S2)
                .rounded(R_SM)
                .bg(t.window)
                .text_size(T_SM)
                .font_family("monospace")
                .text_color(t.text2)
                .child(text("details", format!("{}\n{input}", op.name)))
                .test_support()
        });
        Some(
            card(t)
                .bg(t.raised)
                .border_color(t.accent.opacity(0.5))
                .child(
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
                                .child(heading(pending.button.label.clone(), t))
                                .child(
                                    div()
                                        .text_size(T_SM)
                                        .text_color(t.muted)
                                        .child(text("what", format!("{}. {what}", op.summary()))),
                                ),
                        )
                        .child(buttons),
                )
                .child(
                    div()
                        .id(if pending.plan.is_some() {
                            "pending-plan"
                        } else {
                            "pending-values"
                        })
                        .v_flex()
                        .gap(px(2.))
                        .text_size(T_SM)
                        .text_color(t.text2)
                        .children(lines)
                        .test_support(),
                )
                .child(
                    Button::new("pending-details")
                        .ghost()
                        .small()
                        .label(if pending.details {
                            "Hide details"
                        } else {
                            "Details"
                        })
                        .on_click(cx.listener(|app, _, _, cx| {
                            if let Some(p) = app.state.pending.as_mut() {
                                p.details = !p.details;
                            }
                            cx.notify();
                        })),
                )
                .children(details),
        )
    }

    /// Apply (off when the plan changes nothing) and Discard.
    fn pending_buttons(empty: bool, cx: &mut Context<Self>) -> Div {
        div()
            .h_flex()
            .flex_none()
            .gap(S2)
            .child(
                Button::new("apply")
                    .primary()
                    .label("Apply")
                    .disabled(empty)
                    .on_click(cx.listener(|app, _, _, cx| app.apply(cx))),
            )
            .child(
                Button::new("discard")
                    .label("Discard")
                    .on_click(cx.listener(|app, _, _, cx| app.discard(cx))),
            )
    }

    /// The pending action's lines: the dry run's plan, or the input as labelled values.
    fn pending_lines(
        pending: &crate::Pending,
        op: &legionctl_api::OpInfo,
        empty: bool,
        t: &Tokens,
    ) -> Vec<AnyElement> {
        match &pending.plan {
            Some(_) if empty => vec![div().child("Nothing to change").into_any_element()],
            Some(plan) => plan
                .lines()
                .enumerate()
                .map(|(i, l)| {
                    div()
                        .child(text(format!("line-{i}"), l.to_string()))
                        .into_any_element()
                })
                .collect(),
            None => crate::forms::values(op, &pending.button.input)
                .into_iter()
                .enumerate()
                .map(|(i, (label, value))| {
                    div()
                        .h_flex()
                        .gap(S3)
                        .child(
                            div()
                                .w(px(140.))
                                .flex_none()
                                .text_color(t.muted)
                                .child(label),
                        )
                        .child(div().min_w_0().child(text(format!("value-{i}"), value)))
                        .into_any_element()
                })
                .collect(),
        }
    }

    /// The operator is off: why, the line that turns it on, and Turn on, which does it.
    fn offline(why: &str, failed: Option<&String>, t: &Tokens, cx: &mut Context<Self>) -> Div {
        card(t)
            .max_w(px(640.))
            .child(
                div()
                    .h_flex()
                    .gap(S2)
                    .child(icon(IconName::CircleX, px(18.), t.critical))
                    .child(heading("legionctl-operator is off", t)),
            )
            .child(
                div()
                    .text_size(T_SM)
                    .text_color(t.text2)
                    .child(text("why", why.to_string())),
            )
            .child(
                div().h_flex().child(
                    Button::new("turn-on")
                        .primary()
                        .label("Turn on")
                        .on_click(cx.listener(|app, _, _, cx| app.turn_on(cx))),
                ),
            )
            .children(failed.map(|f| {
                div()
                    .text_size(T_SM)
                    .text_color(t.critical)
                    .child(text("turn-on-failed", f.clone()))
            }))
    }
}

impl Render for LegionApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::of(cx);
        let place = self.tab().map_or_else(
            || self.screen.space.title().to_string(),
            |tab| self.tab_title(tab),
        );
        let title = TitleBar::new().child(
            div()
                .flex_1()
                .h_flex()
                .gap(S2)
                .pr(S3)
                .text_size(T_SM)
                .child(
                    div()
                        .size(px(22.))
                        .flex_none()
                        .rounded(R_SM)
                        .bg(t.accent)
                        .h_flex()
                        .justify_center()
                        .child(icon(IconName::Cpu, px(14.), gpui_kit::white())),
                )
                .child(div().font_semibold().text_color(t.text).child("legionctl"))
                .child(div().text_color(t.muted).child("/"))
                .child(div().min_w_0().truncate().text_color(t.text2).child(place))
                .child(div().flex_1())
                .child(self.place(&t, cx))
                .child(
                    Button::new("feedback")
                        .ghost()
                        .xsmall()
                        .icon(IconName::MessageSquare)
                        .label("Feedback")
                        .tooltip("Write a note about this tab, with a screenshot")
                        .on_click(cx.listener(|app, _, window, cx| app.ask_feedback(window, cx))),
                )
                .child(LegionApp::help_menu(cx))
                .child(self.status(&t)),
        );
        // the tab's content width: the window less the rail, the navigator and the tab's padding
        let viewport = f32::from(window.viewport_size().width);
        let navigator = if self.screen.navigator_open {
            viewport * self.screen.navigator
        } else {
            0.
        };
        let width = (viewport - crate::shell::RAIL - navigator - 48.).clamp(200., 1180.);
        let mut content = div().v_flex().gap(S5);
        if let Some(why) = &self.state.offline {
            content = content.child(LegionApp::offline(why, self.state.turn_on.as_ref(), &t, cx));
        } else {
            content = content
                .children(self.form(&t, window, cx))
                .children(self.pending(&t, cx))
                .child(self.tab_content(&t, width, cx));
            if let Some(message) = &self.state.message {
                content = content.child(crate::widgets::message(message, &t));
            }
        }
        let body = self.tab_body(content, &t, window, cx);
        div()
            .size_full()
            .v_flex()
            .bg(t.window)
            .text_color(t.text)
            .text_size(T_SM)
            .key_context(crate::KEYS)
            .on_key_down(cx.listener(LegionApp::key))
            .child(title)
            .child(self.panes(body, &t, cx))
    }
}
