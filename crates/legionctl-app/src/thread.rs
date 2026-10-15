//! The thread screen of the app (design 23, Part C, P8), the same as `legionctl tui`: pane 1 lists the pages and the
//! thread's view as icons, pane 2 shows the thread's log and the runs of this screen, pane 3 shows the page or the
//! view (C4). Panes 1 and 2 fold to a 24 px rail. The chat bar is at the bottom centre of pane 3: 60% of its width
//! at the start, wider and narrower with Ctrl+Right and Ctrl+Left, taller with its text up to 40% of the pane. The
//! `/` commands and the prompts are the TUI's (`legionctl_api::chat`). The screen reads the thread and its view
//! again at each event of the operator's event stream, not on a timer.

use std::path::PathBuf;

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::text::TextView;
use gpui_kit::{
    AnyElement, AsyncApp, Context, Div, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    WeakEntity, Window, div, px, relative,
};
use legionctl_api::chat::{self, Action, Entry};
use legionctl_api::{
    AgentCommand, ClientPath, Empty, RunOutput, SessionIdIn, ThreadIdIn, ThreadView, ViewData,
    ViewRenderIn, ViewWidget, op,
};
use serde_json::{Value, json};

use crate::{LegionApp, Page};

/// A view of pane 3 (`views.place`, or `/view <name>`): its name, `name=value` parameters and the folder whose
/// repository layer applies.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub name: String,
    pub params: Vec<String>,
    pub repo: PathBuf,
}

/// What the thread screen knows; the operator holds the rest (C6: a restart reads it again).
#[derive(Debug)]
pub struct Screen {
    /// Panes 1 and 2.
    pub folded: [bool; 2],
    pub chat: String,
    /// The chat bar's width in percent of pane 3.
    pub chat_width: u32,
    /// The newest open thread.
    pub thread: Option<ThreadView>,
    pub thread_error: Option<String>,
    /// This screen's lines that ran, each with its result once it ended.
    pub runs: Vec<(String, Option<bool>)>,
    /// The attached agent's commands (from its last run).
    pub commands: Vec<AgentCommand>,
    /// The session and the ACP session of the agent's last run: the next prompt goes on in it.
    pub attached: Option<(String, Option<String>)>,
    /// The chat bar's agent (`--agent`), its dossier, and the repository of its runs.
    pub agent: Option<String>,
    pub dossier: Option<String>,
    pub repo: PathBuf,
    /// The view in pane 3 instead of the page, and its data.
    pub view: Option<Shown>,
    pub view_data: Option<ViewData>,
    /// The log id of the thread's newest `views.place` that this screen applied.
    pub placed: i64,
    /// The events that this screen got from the stream.
    pub events: usize,
}

impl Screen {
    #[must_use]
    pub fn new(repo: PathBuf) -> Screen {
        Screen {
            folded: [false; 2],
            chat: String::new(),
            chat_width: 60,
            thread: None,
            thread_error: None,
            runs: Vec::new(),
            commands: Vec::new(),
            attached: None,
            agent: None,
            dossier: None,
            repo,
            view: None,
            view_data: None,
            placed: 0,
            events: 0,
        }
    }

    /// The `/` commands that match the chat bar's text: the agent's first.
    #[must_use]
    pub fn completions(&self) -> Vec<Entry> {
        let screen = ["1", "2"]
            .iter()
            .map(|n| (format!("/fold {n}"), format!("Fold or open pane {n}")))
            .chain(Page::ALL.iter().map(|p| {
                let name = format!("{p:?}").to_lowercase();
                (format!("/view {name}"), format!("Show {name} in pane 3"))
            }))
            .collect();
        chat::completions(&self.chat, chat::entries(&self.commands, screen))
    }
}

impl LegionApp {
    /// Read the newest open thread, and apply a new placement of its view.
    pub(crate) fn read_thread(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            let newest = client
                .call::<op::ThreadList>(&Empty {})
                .await?
                .into_iter()
                .find(|t| t.state == "open");
            match newest {
                Some(t) => Ok(Some(
                    client
                        .call::<op::ThreadShow>(&ThreadIdIn { thread: t.id })
                        .await?,
                )),
                None => Ok(None),
            }
        });
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| {
                    match result {
                        Ok(thread) => {
                            app.screen.thread = thread;
                            app.screen.thread_error = None;
                            app.apply_layout(cx);
                        }
                        Err(e) => app.screen.thread_error = Some(format!("{e:#}")),
                    }
                    cx.notify();
                });
            },
        )
        .detach();
    }

    /// The thread's layout: its newest `views.place` row, once.
    fn apply_layout(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.screen.thread.as_ref().and_then(|t| {
            t.log
                .iter()
                .rev()
                .find(|r| r.kind == "views.place")
                .cloned()
        }) else {
            return;
        };
        if row.id == self.screen.placed {
            return;
        }
        self.screen.placed = row.id;
        let p = &row.payload;
        let params = p["params"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
            .collect();
        self.show_view(
            Shown {
                name: p["view"].as_str().unwrap_or_default().to_string(),
                params,
                repo: PathBuf::from(p["repo"].as_str().unwrap_or("/")),
            },
            cx,
        );
    }

    pub(crate) fn show_view(&mut self, shown: Shown, cx: &mut Context<Self>) {
        self.screen.view = Some(shown);
        self.render_view(cx);
    }

    /// Read the view of pane 3 again.
    fn render_view(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = self.screen.view.clone() else {
            return;
        };
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            client
                .call::<op::ViewsRender>(&ViewRenderIn {
                    name: shown.name,
                    params: shown.params,
                    repo: ClientPath(shown.repo),
                })
                .await
        });
        self.finish(cx, task, |app, data| app.screen.view_data = Some(data));
    }

    /// Events of the operator's stream: the thread again, and the view when an event refreshes it.
    pub(crate) fn events(&mut self, kinds: &[String], cx: &mut Context<Self>) {
        self.screen.events += kinds.len();
        self.read_thread(cx);
        if self
            .screen
            .view_data
            .as_ref()
            .is_some_and(|v| kinds.iter().any(|k| v.refreshes_on(k)))
        {
            self.render_view(cx);
        }
    }

    /// Run `op` with `input` as the line `line` of pane 2; its text goes to the message line.
    fn start(&mut self, op: &'static str, input: Value, line: String, cx: &mut Context<Self>) {
        self.screen.runs.push((line, None));
        let row = self.screen.runs.len() - 1;
        let client = self.client.clone();
        let task = self.operator(cx, async move { client.call_value(op, &input).await });
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| {
                    let (text, ok) = match result {
                        Ok(value) => {
                            if op == "session.run"
                                && let Ok(run) = serde_json::from_value::<RunOutput>(value.clone())
                            {
                                app.attach(&run, cx);
                            }
                            legionctl_api::render(op, value)
                                .unwrap_or_else(|e| (format!("{e:#}"), false))
                        }
                        Err(e) => (format!("{e:#}"), false),
                    };
                    app.state.message = Some(text);
                    app.screen.runs[row].1 = Some(ok);
                    cx.notify();
                });
            },
        )
        .detach();
    }

    /// The agent's run: its session is the chat bar's now, and its commands are offered.
    fn attach(&mut self, run: &RunOutput, cx: &mut Context<Self>) {
        self.screen.attached = Some((run.id.clone(), run.acp.clone()));
        let client = self.client.clone();
        let input = SessionIdIn { id: run.id.clone() };
        let task = self.operator(cx, async move {
            client.call::<op::SessionCommands>(&input).await
        });
        self.finish(cx, task, |app, commands| app.screen.commands = commands);
    }

    /// The chat bar's line: a command, or a prompt for the agent.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        let line = std::mem::take(&mut self.screen.chat).trim().to_string();
        if line.is_empty() {
            return;
        }
        match chat::action(&line, &self.screen.commands, &["fold", "view"]) {
            Action::Prompt(prompt) => self.prompt(&prompt, line, cx),
            Action::Operation(op, input) => self.start(op.name, input, line, cx),
            Action::Screen(words) => self.screen_command(&words, cx),
            Action::Refused(why) => self.state.message = Some(why),
        }
        cx.notify();
    }

    fn screen_command(&mut self, words: &[String], cx: &mut Context<Self>) {
        match words.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["fold", n @ ("1" | "2")] => {
                let i = usize::from(n == "2");
                self.screen.folded[i] = !self.screen.folded[i];
            }
            ["view", name, ..] => {
                if let Some(page) = Page::named(name) {
                    self.open(page, cx);
                } else {
                    let shown = Shown {
                        name: name.to_string(),
                        params: words[2..].to_vec(),
                        repo: self.screen.repo.clone(),
                    };
                    self.show_view(shown, cx);
                }
            }
            _ => {
                self.state.message =
                    Some("/fold 1, /fold 2 or /view <name> [name=value...]".into());
            }
        }
    }

    /// `prompt` to the agent as a headless run on the thread's account, in the agent's last ACP session.
    fn prompt(&mut self, prompt: &str, line: String, cx: &mut Context<Self>) {
        let Some(agent) = self.screen.agent.clone() else {
            self.state.message =
                Some("no agent: start the app with --agent <name> (config session.acp)".into());
            return;
        };
        let input = json!({
            "agent": agent,
            "prompt": prompt,
            "dossier": self.screen.dossier,
            "repo": self.screen.repo,
            "account": self.screen.thread.as_ref().and_then(|t| t.thread.account.clone()),
            "acp": self.screen.attached.as_ref().and_then(|(_, acp)| acp.clone()),
        });
        self.start("session.run", input, line, cx);
    }

    /// A key on the screen: the folds anywhere, the rest in the chat bar.
    pub(crate) fn key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let k = &event.keystroke;
        let control = k.modifiers.control;
        let alt = k.modifiers.alt;
        match k.key.as_str() {
            "1" | "2" if control || alt => {
                let i = usize::from(k.key == "2");
                self.screen.folded[i] = !self.screen.folded[i];
            }
            _ if !self.chat_focus.is_focused(window) => return,
            "enter" if alt || k.modifiers.shift => self.screen.chat.push('\n'),
            "j" if control => self.screen.chat.push('\n'),
            "enter" => self.submit(cx),
            "right" if control => self.screen.chat_width = (self.screen.chat_width + 10).min(100),
            "left" if control => {
                self.screen.chat_width = self.screen.chat_width.saturating_sub(10).max(30);
            }
            "backspace" => {
                self.screen.chat.pop();
            }
            "tab" => {
                if let Some(first) = self.screen.completions().first()
                    && first.text.starts_with(&self.screen.chat)
                {
                    self.screen.chat = format!("{} ", first.text);
                }
            }
            _ => match &k.key_char {
                Some(text) if !control && !alt => self.screen.chat.push_str(text),
                _ => return,
            },
        }
        cx.stop_propagation();
        cx.notify();
    }
}

/// A folded pane: a 24 px rail with its number and name top to bottom; a click opens it.
fn rail(id: &'static str, label: &str, cx: &mut Context<LegionApp>, pane: usize) -> AnyElement {
    div()
        .id(id)
        .w(px(24.))
        .h_full()
        .flex_none()
        .v_flex()
        .items_center()
        .pt_2()
        .bg(cx.theme().muted)
        .text_xs()
        .children(label.chars().map(|c| div().child(c.to_string())))
        .on_click(cx.listener(move |app, _, _, cx| {
            app.screen.folded[pane] = false;
            cx.notify();
        }))
        .test_support()
        .into_any_element()
}

/// A pane's header: its title and the button that folds it.
fn header(title: String, fold: &'static str, pane: usize, cx: &mut Context<LegionApp>) -> Div {
    div()
        .h_flex()
        .justify_between()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(title)
        .child(
            div()
                .id(fold)
                .px_1()
                .child("«")
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.screen.folded[pane] = true;
                    cx.notify();
                }))
                .test_support(),
        )
}

const ICONS: [(Page, &str); 5] = [
    (Page::Now, "◉"),
    (Page::Gpu, "▣"),
    (Page::Health, "✚"),
    (Page::Bench, "◷"),
    (Page::Resources, "≣"),
];

impl LegionApp {
    /// Pane 1: an icon for each page, and one for the thread's view when it has one.
    fn pane1(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.screen.folded[0] {
            return rail("pane-1", "1views", cx, 0);
        }
        let selected = |page: Page| self.screen.view.is_none() && self.page == page;
        let mut icons: Vec<AnyElement> = ICONS
            .iter()
            .enumerate()
            .map(|(i, (page, icon))| {
                let page = *page;
                div()
                    .id(i)
                    .p_1()
                    .rounded_md()
                    .text_center()
                    .when_selected(selected(page), cx)
                    .child(*icon)
                    .child(div().text_xs().truncate().child(page.title()))
                    .on_click(cx.listener(move |app, _, _, cx| app.open(page, cx)))
                    .test_support()
                    .into_any_element()
            })
            .collect();
        if let Some(shown) = &self.screen.view {
            icons.push(
                div()
                    .id(ICONS.len())
                    .p_1()
                    .rounded_md()
                    .text_center()
                    .when_selected(true, cx)
                    .child("▦")
                    .child(div().text_xs().child(shown.name.clone()))
                    .test_support()
                    .into_any_element(),
            );
        }
        div()
            .id("pane-1")
            .w(relative(0.07))
            .flex_none()
            .h_full()
            .v_flex()
            .gap_1()
            .p_1()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(header("1".into(), "fold-1", 0, cx))
            .child(
                div()
                    .id("pages")
                    .v_flex()
                    .gap_1()
                    .children(icons)
                    .test_support(),
            )
            .test_support()
            .into_any_element()
    }

    /// Pane 2: the thread's log, then this screen's runs.
    fn pane2(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.screen.folded[1] {
            return rail("pane-2", "2events", cx, 1);
        }
        let (title, mut lines) = match (&self.screen.thread, &self.screen.thread_error) {
            (_, Some(e)) => ("2 events".to_string(), vec![format!("the thread: {e}")]),
            (Some(t), None) => (
                format!(
                    "2 thread {}: {}{}",
                    t.thread.id,
                    t.thread.title,
                    t.thread
                        .account
                        .as_ref()
                        .map_or_else(String::new, |a| format!(" (account {a})"))
                ),
                t.log.iter().map(chat::log_line).collect(),
            ),
            (None, None) => ("2 events".to_string(), vec!["no open thread".to_string()]),
        };
        if !self.screen.runs.is_empty() {
            lines.push("runs".into());
            lines.extend(self.screen.runs.iter().map(|(line, ok)| {
                let mark = match ok {
                    None => "…",
                    Some(true) => "✓",
                    Some(false) => "✗",
                };
                format!("{mark} {}", line.replace('\n', " ↵ "))
            }));
        }
        div()
            .id("pane-2")
            .w(relative(0.30))
            .flex_none()
            .h_full()
            .v_flex()
            .gap_1()
            .p_2()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(header(title, "fold-2", 1, cx))
            .child(
                div()
                    .id("events")
                    .flex_1()
                    .overflow_y_scroll()
                    .v_flex()
                    .text_xs()
                    .children(lines.into_iter().map(|l| div().child(l))),
            )
            .test_support()
            .into_any_element()
    }

    /// The view of pane 3 with its widget.
    pub(crate) fn view(&self, cx: &mut Context<Self>) -> Div {
        match &self.screen.view_data {
            Some(data) => widget(data, cx),
            None => div().child("reading the view…"),
        }
    }

    /// Pane 3 with the chat bar at its bottom centre and the completion list above the bar.
    fn pane3(&self, body: Div, cx: &mut Context<Self>) -> AnyElement {
        let mut text = self.screen.chat.clone();
        text.push('▌');
        let bar = div()
            .id("chat")
            .track_focus(&self.chat_focus)
            .w(relative(
                f32::from(u16::try_from(self.screen.chat_width).unwrap_or(100)) / 100.,
            ))
            .max_h(relative(0.4))
            .overflow_hidden()
            .v_flex()
            .justify_end()
            .p_2()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .text_sm()
            .children(text.split('\n').map(|l| div().child(l.to_string())))
            .on_click(cx.listener(|app, _, window, cx| app.chat_focus.focus(window, cx)))
            .test_support();
        let entries = self.screen.completions();
        let list = (!entries.is_empty()).then(|| {
            div()
                .id("completions")
                .w(relative(
                    f32::from(u16::try_from(self.screen.chat_width).unwrap_or(100)) / 100.,
                ))
                .v_flex()
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .text_xs()
                .children(entries.iter().take(12).enumerate().map(|(i, e)| {
                    let row = div()
                        .id(SharedString::from(format!("completion-{i}")))
                        .h_flex()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(e.text.clone()),
                        )
                        .child(e.about.clone());
                    if e.agent {
                        row.text_color(cx.theme().primary)
                            .test_support()
                            .into_any_element()
                    } else {
                        row.test_support().into_any_element()
                    }
                }))
                .test_support()
        });
        div()
            .id("pane-3")
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .h_full()
            .relative()
            .child(
                div()
                    .id("body")
                    .size_full()
                    .overflow_y_scroll()
                    .v_flex()
                    .gap_3()
                    .p_4()
                    .pb(px(120.))
                    .child(body),
            )
            .child(
                div()
                    .absolute()
                    .bottom_2()
                    .left_0()
                    .right_0()
                    .top_0()
                    .v_flex()
                    .justify_end()
                    .items_center()
                    .gap_1()
                    .children(list)
                    .child(bar),
            )
            .test_support()
            .into_any_element()
    }

    /// The three panes.
    pub(crate) fn panes(&self, body: Div, cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .h_flex()
            .child(self.pane1(cx))
            .child(self.pane2(cx))
            .child(self.pane3(body, cx))
    }
}

/// The selected icon's colours.
trait Selected {
    fn when_selected(self, selected: bool, cx: &Context<LegionApp>) -> Self;
}

impl Selected for gpui_kit::Stateful<Div> {
    fn when_selected(self, selected: bool, cx: &Context<LegionApp>) -> Self {
        if selected {
            self.bg(cx.theme().accent)
        } else {
            self
        }
    }
}

/// A view's widget, or the error in its frame (C6).
fn widget(data: &ViewData, cx: &Context<LegionApp>) -> Div {
    let frame = div()
        .v_flex()
        .gap_1()
        .p_2()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .child(div().font_semibold().child(data.name.clone()));
    if let Some(error) = &data.error {
        return frame.child(
            div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(format!("cannot read the view: {error}")),
        );
    }
    let id = |part: &str| SharedString::from(format!("view-{}-{part}", data.name));
    let body: AnyElement = match data.widget {
        ViewWidget::Table => div()
            .id(id("table"))
            .v_flex()
            .text_sm()
            .child(cells(&data.columns, cx).font_semibold())
            .children(
                data.rows
                    .iter()
                    .enumerate()
                    .map(|(i, r)| cells(r, cx).id(id(&format!("row-{i}"))).test_support()),
            )
            .test_support()
            .into_any_element(),
        ViewWidget::List | ViewWidget::Tree => div()
            .id(id(data.widget.to_string().as_str()))
            .v_flex()
            .text_sm()
            .children(data.rows.iter().enumerate().map(|(i, r)| {
                div()
                    .id(id(&format!("row-{i}")))
                    .whitespace_nowrap()
                    .child(r.join(" "))
                    .test_support()
            }))
            .test_support()
            .into_any_element(),
        ViewWidget::Gauge => gauge(data, id("gauge"), cx),
        ViewWidget::Sparkline => div()
            .id(id("sparkline"))
            .text_lg()
            .child(legionctl_api::sparkline(&data.numbers))
            .test_support()
            .into_any_element(),
        ViewWidget::Markdown => {
            TextView::markdown(id("markdown"), data.text.clone()).into_any_element()
        }
        ViewWidget::Diff => div()
            .id(id("diff"))
            .v_flex()
            .text_xs()
            .children(data.text.lines().map(|l| {
                let line = div().child(l.to_string());
                if l.starts_with('+') {
                    line.text_color(cx.theme().success)
                } else if l.starts_with('-') {
                    line.text_color(cx.theme().danger)
                } else {
                    line
                }
            }))
            .test_support()
            .into_any_element(),
        ViewWidget::Stack => div()
            .id(id("stack"))
            .v_flex()
            .gap_2()
            .children(data.parts.iter().map(|p| widget(p, cx)))
            .test_support()
            .into_any_element(),
    };
    frame.child(body)
}

/// A gauge: a bar filled to the value's part of `max`, and the numbers.
fn gauge(data: &ViewData, id: SharedString, cx: &Context<LegionApp>) -> AnyElement {
    let value = data.numbers.first().copied().unwrap_or_default();
    let ratio = value / data.max;
    let percent = (1..=100u16)
        .take_while(|i| f64::from(*i) <= ratio * 100.0)
        .count();
    let fraction = f32::from(u16::try_from(percent).unwrap_or(100)) / 100.;
    div()
        .id(id)
        .v_flex()
        .gap_1()
        .child(
            div()
                .h(px(8.))
                .w_full()
                .rounded_md()
                .bg(cx.theme().muted)
                .child(
                    div()
                        .h_full()
                        .w(relative(fraction))
                        .rounded_md()
                        .bg(cx.theme().primary),
                ),
        )
        .child(div().text_xs().child(format!("{value} / {}", data.max)))
        .test_support()
        .into_any_element()
}

/// One row of a table: a cell of equal width for each value.
fn cells(values: &[String], cx: &Context<LegionApp>) -> Div {
    let n = u16::try_from(values.len().max(1)).unwrap_or(u16::MAX);
    div()
        .h_flex()
        .border_b_1()
        .border_color(cx.theme().border)
        .children(values.iter().map(|v| {
            div()
                .w(relative(1. / f32::from(n)))
                .truncate()
                .child(v.clone())
        }))
}
