//! The window around the tabs (DESIGN-09): a 56 px rail of the spaces (Machine, Work, Agents, Activity), the
//! navigator of the space (pane 2: 30% of the window by default, its edge drags its width), and pane 3: the tabs, each
//! with its own chat bar below its content. The tabs, the space and the navigator's width are facts of the entity
//! `app` (DESIGN-10), written by the user's own clicks and read again at the next start.

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, InteractiveElement as _, IntoElement, MouseButton, MouseMoveEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px, relative,
};
use legionctl_api::{FactSetIn, FactsAtIn, op};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::LegionApp;
use crate::theme::{R_SM, S1, S2, S3, T_SM, T_XS, Tokens, badge, icon};

/// The rail's width.
pub const RAIL: f32 = 56.;
/// The navigator's width (a part of the window) at the first start, and its limits.
pub(crate) const NAVIGATOR: f32 = 0.30;
const NAVIGATOR_LIMITS: (f32, f32) = (0.15, 0.6);

/// A kind of work, with its own navigator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Space {
    Machine,
    Work,
    Agents,
    Activity,
}

impl Space {
    pub const ALL: [Space; 4] = [Space::Machine, Space::Work, Space::Agents, Space::Activity];

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Space::Machine => "Machine",
            Space::Work => "Work",
            Space::Agents => "Agents",
            Space::Activity => "Activity",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Space::Machine => IconName::Cpu,
            Space::Work => IconName::FolderGit2,
            Space::Agents => IconName::Bot,
            Space::Activity => IconName::Activity,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Space::Machine => "machine",
            Space::Work => "work",
            Space::Agents => "agents",
            Space::Activity => "activity",
        }
    }
}

/// Milliseconds since the epoch now.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// What a tab shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Tab {
    View {
        name: String,
        #[serde(default)]
        params: Vec<String>,
    },
    /// A thread: the user's own chat, or another's thread that the user opened.
    Thread { id: i64 },
    /// A new chat: its first message makes the user's thread on the user's dossier, and the tab becomes its tab.
    Chat,
}

impl Tab {
    /// A stable key of the tab (element ids, the data of its view).
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Tab::View { name, params } => format!("view:{name}:{}", params.join(",")),
            Tab::Thread { id } => format!("thread:{id}"),
            Tab::Chat => "chat".into(),
        }
    }
}

/// What a drag moves: the navigator's edge, or a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drag {
    Navigator,
    Tab(usize),
}

/// The window's layout: the persisted part of the screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

impl LegionApp {
    /// The layout facts (entity `app`): tabs, space and navigator; the defaults where there is none.
    pub(crate) fn read_layout(&mut self, open: Option<Tab>, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let input = FactsAtIn {
            entity: Some("app".into()),
            dossier: None,
            thread: None,
            time: None,
            recorded: None,
            rev: None,
            winners: true,
        };
        let task = self.operator(cx, async move { client.call::<op::FactsAt>(&input).await });
        self.finish_with(cx, task, move |app, facts, cx| {
            let value = |attribute: &str| {
                facts
                    .iter()
                    .find(|f| f.attribute == attribute)
                    .map(|f| f.value.clone())
            };
            app.screen.early = std::mem::take(&mut app.screen.tabs);
            // a kept value that this app cannot read is shown, and the default holds until the next change
            let mut bad = Vec::new();
            match value("tabs").map(serde_json::from_value::<Layout>) {
                Some(Ok(layout)) => {
                    app.screen.active = layout.active.min(layout.tabs.len().saturating_sub(1));
                    app.screen.tabs = layout.tabs;
                }
                Some(Err(e)) => bad.push(format!("tabs: {e}")),
                None => {}
            }
            match value("space").map(serde_json::from_value::<Space>) {
                Some(Ok(space)) => app.screen.space = space,
                Some(Err(e)) => bad.push(format!("space: {e}")),
                None => {}
            }
            if !bad.is_empty() {
                app.state.message =
                    Some(format!("Cannot read the saved layout: {}", bad.join("; ")));
            }
            if let Some(nav) = value("navigator") {
                if let Ok(width) = serde_json::from_value::<f32>(nav["width"].clone()) {
                    app.screen.navigator = width.clamp(NAVIGATOR_LIMITS.0, NAVIGATOR_LIMITS.1);
                }
                app.screen.navigator_open = nav["open"].as_bool().unwrap_or(true);
            }
            app.screen.restored = true;
            // tabs that the user opened before the facts came stay, after the kept ones
            let early = std::mem::take(&mut app.screen.early);
            for tab in early.into_iter().chain(open) {
                app.open_tab(tab, cx);
            }
            if app.screen.tabs.is_empty() {
                app.open_view(crate::FIRST, cx);
            }
            app.load_tab(cx);
        });
    }

    /// Write one layout fact of the user.
    pub(crate) fn remember(
        &mut self,
        attribute: &'static str,
        value: &Value,
        cx: &mut Context<Self>,
    ) {
        if !self.screen.restored {
            return;
        }
        let client = self.client.clone();
        let input = FactSetIn {
            entity: "app".into(),
            attribute: attribute.into(),
            value: value.to_string(),
            scope: legionctl_api::FactScope::Global,
            dossier: None,
            thread: None,
            // the time of the change, not of the write: writes run in parallel and may arrive out of order
            valid_from: Some(now_ms()),
            valid_to: None,
            from_ref: None,
            to_ref: None,
            anchor: None,
            evidence: Some("the app's layout".into()),
            confidence: 1.0,
            corrects: None,
        };
        let task = self.operator(cx, async move { client.call::<op::FactsSet>(&input).await });
        cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                let _ = this.update(cx, |app: &mut LegionApp, cx| {
                    app.state.message = Some(format!("Cannot save the layout: {e:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(crate) fn remember_tabs(&mut self, cx: &mut Context<Self>) {
        let layout = Layout {
            tabs: self.screen.tabs.clone(),
            active: self.screen.active,
        };
        self.remember(
            "tabs",
            &serde_json::to_value(layout).unwrap_or_default(),
            cx,
        );
    }

    fn remember_navigator(&mut self, cx: &mut Context<Self>) {
        let value = json!({"width": self.screen.navigator, "open": self.screen.navigator_open});
        self.remember("navigator", &value, cx);
    }

    /// Open `tab`, or show it when it is open.
    pub fn open_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        let before = self.tab().cloned();
        if let Some(i) = self.screen.tabs.iter().position(|t| *t == tab) {
            self.screen.active = i;
        } else {
            self.screen.tabs.push(tab);
            self.screen.active = self.screen.tabs.len() - 1;
        }
        // the last result belongs to the tab where it came
        if self.tab() != before.as_ref() {
            self.state.message = None;
        }
        self.remember_tabs(cx);
        self.load_tab(cx);
        cx.notify();
    }

    pub fn close_tab(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.screen.tabs.len() {
            return;
        }
        let shown = i == self.screen.active;
        self.screen.tabs.remove(i);
        if self.screen.active > i || self.screen.active >= self.screen.tabs.len() {
            self.screen.active = self.screen.active.saturating_sub(1);
        }
        if shown {
            self.state.message = None;
        }
        self.remember_tabs(cx);
        self.load_tab(cx);
        cx.notify();
    }

    /// Move tab `from` to the place of tab `to`; the moved tab stays active.
    pub fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let n = self.screen.tabs.len();
        if from == to || from >= n || to >= n {
            return;
        }
        let tab = self.screen.tabs.remove(from);
        self.screen.tabs.insert(to, tab);
        self.screen.active = to;
        self.remember_tabs(cx);
        cx.notify();
    }

    pub fn show_tab(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.screen.tabs.len() && i != self.screen.active {
            self.screen.active = i;
            self.state.message = None;
            self.remember_tabs(cx);
            self.load_tab(cx);
        }
        cx.notify();
    }

    /// The space of the rail; a click on the space that shows folds or opens the navigator.
    pub fn show_space(&mut self, space: Space, cx: &mut Context<Self>) {
        if self.screen.space == space {
            self.screen.navigator_open = !self.screen.navigator_open;
            self.remember_navigator(cx);
        } else {
            self.screen.space = space;
            self.screen.navigator_open = true;
            self.remember("space", &json!(space), cx);
            self.remember_navigator(cx);
            self.load_space(cx);
        }
        cx.notify();
    }

    /// The tab that shows, if any.
    #[must_use]
    pub fn tab(&self) -> Option<&Tab> {
        self.screen.tabs.get(self.screen.active)
    }

    fn drag_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen.drag != Some(Drag::Navigator)
            || event.pressed_button != Some(MouseButton::Left)
        {
            return;
        }
        let width = f32::from(window.viewport_size().width);
        let part = (f32::from(event.position.x) - RAIL) / width.max(1.);
        self.screen.navigator = part.clamp(NAVIGATOR_LIMITS.0, NAVIGATOR_LIMITS.1);
        cx.notify();
    }

    fn drag_end(&mut self, cx: &mut Context<Self>) {
        if self.screen.drag.take() == Some(Drag::Navigator) {
            self.remember_navigator(cx);
        }
        cx.notify();
    }

    /// The rail: one icon per space, its name in the tooltip; the space that shows has the accent.
    fn rail(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let attention = self.screen.activity.attention();
        div()
            .id("rail")
            .w(px(RAIL))
            .flex_none()
            .h_full()
            .v_flex()
            .items_center()
            .gap(S1)
            .pt(S2)
            .bg(t.pane)
            .border_r_1()
            .border_color(t.border)
            .children(Space::ALL.into_iter().map(|space| {
                let selected = self.screen.space == space;
                let color = if selected { t.accent } else { t.text2 };
                let tip = format!("{} (Ctrl+{})", space.title(), space as usize + 1);
                crate::theme::control(
                    div().id(SharedString::from(format!("space-{}", space.word()))),
                    space.title(),
                    t,
                )
                .relative()
                .size(px(40.))
                .h_flex()
                .justify_center()
                .rounded(R_SM)
                .cursor_pointer()
                .when(selected, |d| d.bg(t.accent.opacity(0.14)))
                .when(!selected, |d| d.hover(|s| s.bg(t.card)))
                .child(icon(space.icon(), px(20.), color))
                .when(space == Space::Activity && attention > 0, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(4.))
                            .right(px(4.))
                            .size(px(8.))
                            .rounded_full()
                            .bg(t.warn),
                    )
                })
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(move |app, _, _, cx| app.show_space(space, cx)))
                .test_support()
                .into_any_element()
            }))
            .test_support()
            .into_any_element()
    }

    /// Pane 2: the navigator of the space, and its edge that drags its width.
    fn navigator(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.screen.navigator_open {
            return None;
        }
        let entries = self.navigator_entries(t, cx);
        Some(
            div()
                .id("navigator")
                .relative()
                .w(relative(self.screen.navigator))
                .flex_none()
                .h_full()
                .v_flex()
                .bg(t.pane)
                .border_r_1()
                .border_color(t.border)
                .child(
                    div()
                        .px(S3)
                        .pt(S3)
                        .pb(S2)
                        .text_size(T_XS)
                        .font_semibold()
                        .text_color(t.muted)
                        .child(self.screen.space.title().to_uppercase()),
                )
                .child(
                    div()
                        .id("navigator-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .v_flex()
                        .px(S2)
                        .pb(S3)
                        .children(entries),
                )
                .child(
                    div()
                        .id("navigator-edge")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(px(-3.))
                        .w(px(6.))
                        .cursor_col_resize()
                        .when(self.screen.drag == Some(Drag::Navigator), |d| {
                            d.bg(t.accent.opacity(0.5))
                        })
                        .hover(|s| s.bg(t.accent.opacity(0.3)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|app, _, _, cx| {
                                app.screen.drag = Some(Drag::Navigator);
                                cx.stop_propagation();
                            }),
                        )
                        .test_support(),
                )
                .test_support()
                .into_any_element(),
        )
    }

    /// A section title in the navigator.
    pub(crate) fn nav_section(title: &str, t: &Tokens) -> AnyElement {
        div()
            .pt(S3)
            .pb(S1)
            .px(S2)
            .text_size(T_XS)
            .text_color(t.muted)
            .child(title.to_string())
            .into_any_element()
    }

    /// One entry of the navigator: its icon, name and a short value; the full name in its tooltip.
    /// A navigator entry that opens something: the keyboard reaches it.
    pub(crate) fn nav_button(
        id: String,
        entry: (IconName, String, Option<(String, gpui_kit::Hsla)>),
        selected: bool,
        t: &Tokens,
    ) -> gpui_kit::Stateful<Div> {
        let label = entry.1.clone();
        crate::theme::control(Self::nav_entry(id, entry, selected, t), label, t)
    }

    pub(crate) fn nav_entry(
        id: String,
        (glyph, name, value): (IconName, String, Option<(String, gpui_kit::Hsla)>),
        selected: bool,
        t: &Tokens,
    ) -> gpui_kit::Stateful<Div> {
        let tip = name.clone();
        div()
            .id(SharedString::from(id))
            .h_flex()
            .gap(S2)
            .px(S2)
            .py(px(6.))
            .rounded(R_SM)
            .cursor_pointer()
            .text_size(T_SM)
            .when(selected, |d| d.bg(t.accent.opacity(0.14)))
            .when(!selected, |d| d.hover(|s| s.bg(t.card)))
            .child(icon(
                glyph,
                px(16.),
                if selected { t.accent } else { t.muted },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if selected { t.text } else { t.text2 })
                    .child(name),
            )
            .children(value.map(|(v, color)| badge(v, color)))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
    }

    fn navigator_entries(&self, t: &Tokens, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let active = self.tab().cloned();
        match self.screen.space {
            Space::Machine => self.view_entries("machine", t, cx),
            Space::Work => {
                let mut out = vec![Self::nav_section("Your chats", t)];
                let selected = active.as_ref() == Some(&Tab::Chat);
                out.push(
                    Self::nav_button(
                        "nav-new-chat".into(),
                        (IconName::Plus, "New chat".into(), None),
                        selected,
                        t,
                    )
                    .on_click(cx.listener(|app, _, _, cx| app.open_tab(Tab::Chat, cx)))
                    .test_support()
                    .into_any_element(),
                );
                // the user's own threads, newest first; the agents' threads are in the Agents space
                out.extend(
                    self.screen
                        .threads
                        .iter()
                        .filter(|th| th.owner.is_none())
                        .map(|th| self.thread_entry(th, None, t, cx)),
                );
                out.push(Self::nav_section("Views", t));
                out.extend(self.view_entries("work", t, cx));
                out
            }
            Space::Agents => {
                let now = self.screen.agent();
                let mut out = vec![Self::nav_section("Agents", t)];
                out.extend(self.screen.agents.iter().map(|a| {
                    let picked = now.as_deref() == Some(a.as_str());
                    let name = a.clone();
                    Self::nav_button(
                        format!("nav-agent-{a}"),
                        (
                            IconName::Bot,
                            a.clone(),
                            picked.then(|| ("in use".to_string(), t.accent)),
                        ),
                        false,
                        t,
                    )
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.screen.chat_mut().clone_from(&format!("/agent {name}"));
                        cx.notify();
                    }))
                    .test_support()
                    .into_any_element()
                }));
                out.push(Self::nav_section("Agent chats", t));
                out.extend(self.screen.threads.iter().filter_map(|th| {
                    let owner = th.owner.as_deref()?;
                    Some(self.thread_entry(th, Some(owner), t, cx))
                }));
                out.push(Self::nav_section("Views", t));
                out.extend(self.view_entries("agents", t, cx));
                out
            }
            Space::Activity => self.activity_entries(t, cx),
        }
    }

    /// A thread in the navigator: its title, and its dossier (or its owner's session, for an agent's thread); a click
    /// opens it as a tab.
    fn thread_entry(
        &self,
        th: &legionctl_api::ThreadInfo,
        owner: Option<&str>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tab = Tab::Thread { id: th.id };
        let selected = self.tab() == Some(&tab);
        let state = if th.state == "open" { t.ok } else { t.muted };
        let value = match owner {
            Some(session) => format!("{} · {}", th.dossier, &session[..session.len().min(8)]),
            None => th.dossier.clone(),
        };
        Self::nav_button(
            format!("nav-thread-{}", th.id),
            (
                IconName::MessageSquare,
                th.title.clone(),
                Some((value, state)),
            ),
            selected,
            t,
        )
        .on_click(cx.listener(move |app, _, _, cx| app.open_tab(tab.clone(), cx)))
        .test_support()
        .into_any_element()
    }

    /// The views that the navigator lists (`views.list`): the seeds and the saved views with their space.
    pub(crate) fn read_views(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            client.call::<op::ViewsList>(&legionctl_api::Empty {}).await
        });
        self.finish(cx, task, |app, views| app.screen.views = views);
    }

    /// The navigator's entries of the views of `space` (a view with no space is the agents'); a view whose file is
    /// not a view shows why.
    fn view_entries(&self, space: &str, t: &Tokens, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let active = self.tab().cloned();
        let listed: Vec<&legionctl_api::ViewInfo> = self
            .screen
            .views
            .iter()
            .filter(|v| v.space.as_deref().unwrap_or("agents") == space)
            .collect();
        if listed.is_empty() {
            return vec![Self::nav_note(
                "No view yet. Ask for one in the chat bar.",
                t,
            )];
        }
        listed
            .into_iter()
            .map(|v| {
                let tab = Tab::View {
                    name: v.name.clone(),
                    params: Vec::new(),
                };
                let selected = active.as_ref() == Some(&tab);
                let value = match (&v.error, v.layer.as_str()) {
                    (Some(_), _) => Some(("not a view".to_string(), t.critical)),
                    (None, "user") => Some(("changed".to_string(), t.muted)),
                    (None, "repository") => Some(("from the repository".to_string(), t.muted)),
                    (None, _) => None,
                };
                let glyph = match v.widget {
                    Some(legionctl_api::ViewWidget::Chart) => IconName::ChartLine,
                    Some(legionctl_api::ViewWidget::Table) => IconName::Boxes,
                    Some(legionctl_api::ViewWidget::List) => IconName::HeartPulse,
                    _ => IconName::LayoutDashboard,
                };
                Self::nav_button(
                    format!("nav-{}", v.name),
                    (
                        glyph,
                        v.title.clone().unwrap_or_else(|| v.name.clone()),
                        value,
                    ),
                    selected,
                    t,
                )
                .on_click(cx.listener(move |app, _, _, cx| app.open_tab(tab.clone(), cx)))
                .test_support()
                .into_any_element()
            })
            .collect()
    }

    pub(crate) fn nav_note(text: &str, t: &Tokens) -> AnyElement {
        div()
            .px(S2)
            .py(S1)
            .text_size(T_XS)
            .text_color(t.muted)
            .child(text.to_string())
            .into_any_element()
    }

    /// The tab bar: each tab's name, a close button, a drag to another tab moves it.
    fn tab_bar(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("tabs")
            .h(px(36.))
            .flex_none()
            .h_flex()
            .items_end()
            .gap(px(2.))
            .px(S2)
            .bg(t.pane)
            .border_b_1()
            .border_color(t.border)
            .overflow_x_scroll()
            .children(self.screen.tabs.iter().enumerate().map(|(i, tab)| {
                let selected = i == self.screen.active;
                let title = self.tab_title(tab);
                let tip = title.clone();
                crate::theme::control(
                    div().id(SharedString::from(format!("tab-{i}"))),
                    title.clone(),
                    t,
                )
                // the mouse picks and drags a tab on its button's release; the keys pick it with Enter
                .on_click(cx.listener(move |app, e: &gpui_kit::ClickEvent, _, cx| {
                    if e.is_keyboard() {
                        app.show_tab(i, cx);
                    }
                }))
                .h(px(30.))
                .max_w(px(220.))
                .flex_none()
                .h_flex()
                .gap(S1)
                .pl(S3)
                .pr(S1)
                .rounded_t(R_SM)
                .text_size(T_SM)
                .cursor_pointer()
                .when(selected, |d| {
                    d.bg(t.window)
                        .text_color(t.text)
                        .border_1()
                        .border_b_0()
                        .border_color(t.border)
                })
                .when(!selected, |d| d.text_color(t.muted).hover(|s| s.bg(t.card)))
                .when(
                    matches!(self.screen.drag, Some(Drag::Tab(from)) if from != i),
                    |d| d.border_l_2().border_color(t.accent),
                )
                .child(div().min_w_0().truncate().child(title))
                .child(
                    crate::theme::control(
                        div().id(SharedString::from(format!("tab-close-{i}"))),
                        format!("Close {tip}"),
                        t,
                    )
                    .p(px(2.))
                    .rounded(R_SM)
                    .hover(|s| s.bg(t.raised))
                    .child(icon(IconName::Close, px(12.), t.muted))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |app, _, _, cx| {
                        cx.stop_propagation();
                        app.close_tab(i, cx);
                    }))
                    .test_support(),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |app, _, _, _| app.screen.drag = Some(Drag::Tab(i))),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |app, _, _, cx| {
                        if let Some(Drag::Tab(from)) = app.screen.drag.take() {
                            if from == i {
                                app.show_tab(i, cx);
                            } else {
                                app.move_tab(from, i, cx);
                            }
                        }
                    }),
                )
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .test_support()
                .into_any_element()
            }))
            .test_support()
            .into_any_element()
    }

    /// A tab's name in the user's words.
    pub(crate) fn tab_title(&self, tab: &Tab) -> String {
        match tab {
            Tab::View { name, .. } => self
                .screen
                .view_data
                .get(&tab.key())
                .map(|d| d.title.clone())
                .or_else(|| {
                    let listed = self.screen.views.iter().find(|v| v.name == *name)?;
                    listed.title.clone()
                })
                .unwrap_or_else(|| name.clone()),
            Tab::Thread { id } => self
                .screen
                .threads
                .iter()
                .find(|t| t.id == *id)
                .map_or_else(|| format!("Chat {id}"), |t| t.title.clone()),
            Tab::Chat => "New chat".into(),
        }
    }

    /// The title bar's work place: the user's dossier and the tab's thread, or the choice to make; a click opens the
    /// Dossiers view, where the user chooses, makes or releases one.
    pub(crate) fn place(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let thread = self.screen.thread.as_ref().map(|v| &v.thread);
        let (text, color) = match (&self.screen.dossier, thread) {
            (_, Some(th)) => {
                let repo = match (&th.repo, &th.branch) {
                    (Some(repo), Some(branch)) => {
                        let name = std::path::Path::new(repo)
                            .file_name()
                            .map_or_else(|| repo.clone(), |n| n.to_string_lossy().into_owned());
                        format!("{name}@{branch}")
                    }
                    (Some(folder), None) => folder.clone(),
                    _ => "new folder".into(),
                };
                (
                    format!("dossier {} · chat {} · {repo} · sandbox", th.dossier, th.id),
                    t.accent,
                )
            }
            (Some(d), None) if self.tab() == Some(&Tab::Chat) => {
                let folder = match self.screen.chat_place {
                    Some(_) => self.screen.project.file_name().map_or_else(
                        || self.screen.project.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    ),
                    None => "new folder".to_string(),
                };
                (
                    format!("dossier {d} · new chat · {folder} · sandbox"),
                    t.accent,
                )
            }
            (Some(d), None) => (format!("dossier {d}"), t.accent),
            (None, None) => ("Choose a dossier".to_string(), t.warn),
        };
        crate::theme::control(div().id("place"), "Choose the dossier", t)
            .h_flex()
            .gap(S1)
            .px(S2)
            .py(px(2.))
            .rounded_full()
            .bg(color.opacity(0.14))
            .text_size(T_XS)
            .text_color(color)
            .cursor_pointer()
            .child(icon(IconName::FolderGit2, px(12.), color))
            .child(text)
            .tooltip(|window, cx| Tooltip::new("Choose a dossier").build(window, cx))
            .on_click(cx.listener(|app, _, _, cx| app.open_view("dossiers", cx)))
            .test_support()
            .into_any_element()
    }

    /// The title bar's status (U19): the operator's answer and how old the data is.
    pub(crate) fn status(&self, t: &Tokens) -> AnyElement {
        let (color, text) = match (&self.state.offline, self.screen.updated) {
            (Some(_), _) => (t.critical, "operator is off".to_string()),
            (None, None) => (t.muted, "connecting".to_string()),
            (None, Some(at)) => (
                t.ok,
                format!(
                    "live, updated {}",
                    crate::thread::ago_short(at.elapsed().as_secs())
                ),
            ),
        };
        div()
            .id("status")
            .h_flex()
            .gap(S1)
            .text_size(T_XS)
            .text_color(t.muted)
            .child(div().size(px(8.)).rounded_full().bg(color))
            .child(text)
            .test_support()
            .into_any_element()
    }

    /// The panes: the rail, the navigator and pane 3; the window's mouse moves drag the navigator's edge.
    pub(crate) fn panes(&self, body: AnyElement, t: &Tokens, cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .h_flex()
            .on_mouse_move(cx.listener(LegionApp::drag_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|app, _, _, cx| app.drag_end(cx)),
            )
            .child(self.rail(t, cx))
            .children(self.navigator(t, cx))
            .child(
                div()
                    .id("pane-3")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .v_flex()
                    .bg(t.window)
                    .child(self.tab_bar(t, cx))
                    .child(body)
                    .test_support(),
            )
    }
}
