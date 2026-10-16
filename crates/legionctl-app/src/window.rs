//! The window: the title bar with the operator's status, the panes (`shell`), the plan of a pending action above the
//! tab's content, and the last result below it. The kit's icons and the app's extra icons.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, StyledExt as _, TitleBar};
use gpui_kit::{
    Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Window, div, px,
};
use legionctl_api::Class;

use crate::LegionApp;
use crate::theme::{R_SM, S1, S2, S3, S5, T_SM, Tokens, card, heading, icon};

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
    /// The plan of the pending action, in its class's words, with Apply and Discard (DESIGN-09: a session or
    /// workspace change confirms inline with its plan; a machine change shows its plan, then polkit asks).
    fn pending(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<Div> {
        let pending = self.state.pending.as_ref()?;
        let what = match pending.class {
            Class::Machine => {
                "changes the machine; when you apply it, the system asks for your password"
            }
            Class::Workspace => "changes your files or settings",
            Class::Session => "changes this session's own state",
            Class::Read => "reads only",
        };
        let lines: Vec<String> = match &pending.plan {
            Some(plan) => plan.lines().map(String::from).collect(),
            None => vec![format!("{} {}", pending.button.op, pending.button.input)],
        };
        let empty = pending.plan.as_deref().is_some_and(|p| p.trim().is_empty());
        // the buttons first: a long plan must not push them out of reach
        let buttons = div()
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
            );
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
                                        .child(format!("{}: {what}", pending.button.op)),
                                ),
                        )
                        .child(buttons),
                )
                .child(
                    div()
                        .v_flex()
                        .gap(px(2.))
                        .text_size(T_SM)
                        .text_color(t.text2)
                        .children(if empty {
                            vec![div().child("Nothing: the machine is already in this state")]
                        } else {
                            lines.into_iter().map(|l| div().child(l)).collect()
                        }),
                ),
        )
    }

    /// The operator does not answer: why, and how to start it.
    fn offline(why: &str, t: &Tokens) -> Div {
        card(t)
            .max_w(px(640.))
            .child(
                div()
                    .h_flex()
                    .gap(S2)
                    .child(icon(IconName::CircleX, px(18.), t.critical))
                    .child(heading("legionctl-operator does not answer", t)),
            )
            .child(
                div()
                    .text_size(T_SM)
                    .text_color(t.text2)
                    .child(why.to_string()),
            )
            .child(
                div()
                    .px(S3)
                    .py(S2)
                    .rounded(R_SM)
                    .bg(t.raised)
                    .text_size(T_SM)
                    .font_family("monospace")
                    .text_color(t.text)
                    .child("systemctl --user enable --now legionctl-operator.socket"),
            )
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
            content = content.child(LegionApp::offline(why, &t));
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
            .on_key_down(cx.listener(LegionApp::key))
            .child(title)
            .child(self.panes(body, &t, cx))
    }
}
