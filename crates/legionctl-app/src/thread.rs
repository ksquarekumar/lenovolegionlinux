//! The thread screen of the app (design 23, Part C, P8), the same as `legionctl tui`: pane 1 lists the pages and the
//! thread's view as icons, pane 2 shows the thread's log and the runs of this screen, pane 3 shows the page or the
//! view (C4). Panes 1 and 2 fold to a 24 px rail. The chat bar is at the bottom centre of pane 3: 60% of its width
//! at the start, wider and narrower with Ctrl+Right and Ctrl+Left, taller with its text up to 40% of the pane. The
//! `/` commands and the prompts are the TUI's (`legionctl_api::chat`). The screen reads the thread and its view
//! again at each event of the operator's event stream, not on a timer.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AsyncApp, Context, Div, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    WeakEntity, Window, div, px, relative,
};
use legionctl_api::chat::{self, Action, Entry};
use legionctl_api::{
    AgentCommand, ClientPath, Empty, ProjectIn, RunOutput, SessionIdIn, ThreadIdIn, ThreadLogRow,
    ThreadView, ViewData, ViewRenderIn, ViewWidget, op,
};
use serde_json::{Value, json};

use crate::pages::PAGES;
use crate::theme::{
    R_LG, R_MD, R_SM, S1, S2, S3, S4, S5, T_MD, T_SM, T_XS, Tokens, badge, card, icon,
};
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
    /// The agents of config `session.acp`, and `app.agent`: the chat bar's agent where the thread has no pick.
    pub agents: Vec<String>,
    pub default_agent: Option<String>,
    /// The dossier of the chat bar's runs (`--dossier`), and their repository.
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
            agents: Vec::new(),
            default_agent: None,
            dossier: None,
            repo,
            view: None,
            view_data: None,
            placed: 0,
            events: 0,
        }
    }

    /// The chat bar's agent: the thread's newest `/agent` pick (a `thread.agent` row), else `app.agent`.
    #[must_use]
    pub fn agent(&self) -> Option<String> {
        self.thread
            .iter()
            .flat_map(|t| t.log.iter().rev())
            .find(|r| r.kind == "thread.agent")
            .and_then(|r| r.payload["agent"].as_str().map(String::from))
            .or_else(|| self.default_agent.clone())
    }

    /// The `/` commands that match the chat bar's text: the agent's first.
    #[must_use]
    pub fn completions(&self) -> Vec<Entry> {
        let now = self.agent();
        let screen = ["1", "2"]
            .iter()
            .map(|n| (format!("/fold {n}"), format!("Fold or open pane {n}")))
            .chain(Page::ALL.iter().map(|p| {
                let name = format!("{p:?}").to_lowercase();
                (format!("/view {name}"), format!("Show {name} in pane 3"))
            }))
            .chain(self.agents.iter().map(|a| {
                let current = if now.as_ref() == Some(a) { ", now" } else { "" };
                (
                    format!("/agent {a}"),
                    format!("Send the prompts to {a} (session.acp{current})"),
                )
            }))
            .collect();
        chat::completions(&self.chat, chat::entries(&self.commands, screen))
    }
}

impl LegionApp {
    /// Read the newest open thread and the chat bar's agents (config `session.acp`, `app.agent`), and apply a new
    /// placement of the thread's view.
    pub(crate) fn read_thread(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let project = ProjectIn {
            project: ClientPath(self.screen.repo.clone()),
        };
        let task = self.operator(cx, async move {
            let config = client.call::<op::ConfigShow>(&project).await?.value;
            let newest = client
                .call::<op::ThreadList>(&Empty {})
                .await?
                .into_iter()
                .find(|t| t.state == "open");
            let thread = match newest {
                Some(t) => Some(
                    client
                        .call::<op::ThreadShow>(&ThreadIdIn { thread: t.id })
                        .await?,
                ),
                None => None,
            };
            Ok((thread, config))
        });
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| {
                    match result {
                        Ok((thread, config)) => {
                            app.screen.agents = config["session"]["acp"]
                                .as_object()
                                .map(|acp| acp.keys().cloned().collect())
                                .unwrap_or_default();
                            app.screen.default_agent =
                                config["app"]["agent"].as_str().map(String::from);
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
        match chat::action(&line, &self.screen.commands, &["fold", "view", "agent"]) {
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
            ["agent", name] => match &self.screen.thread {
                Some(t) => {
                    let input =
                        json!({"thread": t.thread.id, "agent": name, "repo": self.screen.repo});
                    self.start("thread.agent", input, format!("/agent {name}"), cx);
                }
                None => {
                    self.state.message =
                        Some("no open thread: /agent keeps its pick in a thread's log".into());
                }
            },
            _ => {
                self.state.message =
                    Some("/fold 1, /fold 2, /view <name> [name=value...] or /agent <name>".into());
            }
        }
    }

    /// `prompt` to the agent as a headless run on the thread's account, in the agent's last ACP session.
    fn prompt(&mut self, prompt: &str, line: String, cx: &mut Context<Self>) {
        let Some(agent) = self.screen.agent() else {
            self.state.message = Some(format!(
                "no agent: pick one with /agent <name> (session.acp: {})",
                self.screen.agents.join(", ")
            ));
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

/// Seconds since the epoch now.
pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// How long ago `at` was, in a short word: now, 5 min, 3 h, 2 d.
pub(crate) fn ago(now: i64, at: i64) -> String {
    match now.saturating_sub(at) {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86_400 => format!("{} h", s / 3600),
        s => format!("{} d", s / 86_400),
    }
}

/// `at` as a date and time in UTC: 2026-10-16 15:18 UTC.
fn utc(at: i64) -> String {
    // the civil date of a day count (Howard Hinnant's days_from_civil, reversed)
    let z = at.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let secs = at.rem_euclid(86_400);
    format!(
        "{year}-{month:02}-{day:02} {:02}:{:02} UTC",
        secs / 3600,
        secs / 60 % 60
    )
}

/// A member's colour, the same for the same name.
fn member_color(name: &str) -> gpui_kit::Hsla {
    let hue = name
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)))
        % 360;
    gpui_kit::hsla(
        f32::from(u16::try_from(hue).unwrap_or(0)) / 360.,
        0.55,
        0.62,
        1.,
    )
}

/// A member's chip: the first letter of its name on its colour.
fn avatar(name: &str, size: f32) -> Div {
    let color = member_color(name);
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(color.opacity(0.22))
        .text_color(color)
        .text_size(px(size * 0.5))
        .font_weight(FontWeight::SEMIBOLD)
        .h_flex()
        .justify_center()
        .child(
            name.chars()
                .next()
                .unwrap_or('·')
                .to_uppercase()
                .to_string(),
        )
}

/// The colour of a log row's kind.
fn kind_color(kind: &str, t: &Tokens) -> gpui_kit::Hsla {
    match kind {
        "start" | "end" | "branch" | "import" | "imported" | "place" | "agent" => t.accent,
        "join" | "checkpoint" | "declare" | "ack" | "wake" => t.ok,
        "rule" | "rule.rm" | "pace" | "teammate" | "backfill" | "compensate" => t.warn,
        "fail" | "lost" | "miss" | "budget" | "cut-off" | "stop" => t.critical,
        _ => t.text2,
    }
}

/// A row of the thread's timeline: the member's chip, its name, the kind, the time (UTC in the tooltip), and what
/// the row says.
fn log_row(r: &ThreadLogRow, now: i64, t: &Tokens) -> AnyElement {
    let kind = r
        .kind
        .trim_start_matches("thread.")
        .trim_start_matches("views.");
    let member = if r.member.is_empty() {
        "legionctl"
    } else {
        &r.member
    };
    let when = utc(r.at);
    div()
        .h_flex()
        .items_start()
        .gap(S2)
        .py(S2)
        .child(avatar(member, 24.))
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    div()
                        .h_flex()
                        .gap(S2)
                        .text_size(T_XS)
                        .child(
                            div()
                                .font_semibold()
                                .text_color(t.text)
                                .child(member.to_string()),
                        )
                        .child(badge(kind.to_string(), kind_color(kind, t)))
                        .child(div().flex_1())
                        .child(
                            div()
                                .id(SharedString::from(format!("at-{}", r.id)))
                                .text_color(t.muted)
                                .child(ago(now, r.at))
                                .tooltip(move |window, cx| {
                                    Tooltip::new(when.clone()).build(window, cx)
                                }),
                        ),
                )
                .child(
                    div()
                        .text_size(T_SM)
                        .text_color(t.text2)
                        .child(chat::log_detail(r)),
                ),
        )
        .into_any_element()
}

/// A folded pane: a 24 px rail with an open icon and its name top to bottom; a click opens it.
fn rail(
    id: &'static str,
    label: &str,
    t: &Tokens,
    cx: &mut Context<LegionApp>,
    pane: usize,
) -> AnyElement {
    let tip = format!("Open {label}");
    div()
        .id(id)
        .w(px(24.))
        .h_full()
        .flex_none()
        .v_flex()
        .items_center()
        .gap(S2)
        .pt(S3)
        .bg(t.pane)
        .border_r_1()
        .border_color(t.border)
        .cursor_pointer()
        .hover(|s| s.bg(t.card))
        .child(icon(IconName::PanelLeftOpen, px(14.), t.muted))
        .child(
            div()
                .v_flex()
                .items_center()
                .text_size(px(10.))
                .text_color(t.muted)
                .children(
                    label
                        .chars()
                        .map(|c| div().child(c.to_uppercase().to_string())),
                ),
        )
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |app, _, _, cx| {
            app.screen.folded[pane] = false;
            cx.notify();
        }))
        .test_support()
        .into_any_element()
}

/// The button that folds a pane.
fn fold(id: &'static str, pane: usize, t: &Tokens, cx: &mut Context<LegionApp>) -> AnyElement {
    let tip = if pane == 0 {
        "Fold the pages (Ctrl+1)"
    } else {
        "Fold the thread (Ctrl+2)"
    };
    div()
        .id(id)
        .flex_none()
        .p(S1)
        .rounded(R_SM)
        .cursor_pointer()
        .hover(|s| s.bg(t.card))
        .child(icon(IconName::PanelLeftClose, px(16.), t.muted))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        .on_click(cx.listener(move |app, _, _, cx| {
            app.screen.folded[pane] = true;
            cx.notify();
        }))
        .test_support()
        .into_any_element()
}

/// One entry of pane 1: its icon and its name, the accent bar when it shows, the full name in its tooltip.
fn entry(
    id: usize,
    glyph: IconName,
    name: &str,
    selected: bool,
    t: &Tokens,
) -> gpui_kit::Stateful<Div> {
    let tip = name.to_string();
    let color = if selected { t.accent } else { t.text2 };
    div()
        .id(id)
        .relative()
        .v_flex()
        .items_center()
        .gap(S1)
        .py(S2)
        .px(S1)
        .rounded(R_SM)
        .cursor_pointer()
        .when(selected, |d| d.bg(t.accent.opacity(0.14)))
        .when(!selected, |d| d.hover(|s| s.bg(t.card)))
        .child(icon(glyph, px(20.), color))
        .child(
            div()
                .w_full()
                .text_center()
                .truncate()
                .text_size(px(11.))
                .text_color(color)
                .child(name.to_string()),
        )
        .when(selected, |d| {
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .top(S2)
                    .bottom(S2)
                    .w(px(3.))
                    .rounded_full()
                    .bg(t.accent),
            )
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
}

impl LegionApp {
    /// Pane 1: an icon for each page, and one for the thread's view when it has one.
    fn pane1(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        if self.screen.folded[0] {
            return rail("pane-1", "pages", t, cx, 0);
        }
        let selected = |page: Page| self.screen.view.is_none() && self.page == page;
        let mut icons: Vec<AnyElement> = PAGES
            .iter()
            .enumerate()
            .map(|(i, (page, glyph))| {
                let page = *page;
                entry(i, *glyph, page.title(), selected(page), t)
                    .on_click(cx.listener(move |app, _, _, cx| app.open(page, cx)))
                    .test_support()
                    .into_any_element()
            })
            .collect();
        if let Some(shown) = &self.screen.view {
            icons.push(
                entry(PAGES.len(), IconName::LayoutDashboard, &shown.name, true, t)
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
            .gap(S2)
            .p(S1)
            .pt(S2)
            .bg(t.pane)
            .border_r_1()
            .border_color(t.border)
            .child(
                div()
                    .h_flex()
                    .justify_center()
                    .child(fold("fold-1", 0, t, cx)),
            )
            .child(
                div()
                    .id("pages")
                    .v_flex()
                    .gap(S1)
                    .children(icons)
                    .test_support(),
            )
            .test_support()
            .into_any_element()
    }

    /// Pane 2's header: the thread's title, its state, account and members, and the fold button.
    fn thread_header(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let (title, facts, members): (String, Vec<Div>, Vec<String>) =
            match (&self.screen.thread, &self.screen.thread_error) {
                (_, Some(e)) => (
                    "Thread".into(),
                    vec![
                        badge("cannot read", t.critical),
                        div().text_color(t.muted).child(e.clone()),
                    ],
                    Vec::new(),
                ),
                (Some(v), None) => {
                    let mut facts = vec![badge(
                        v.thread.state.clone(),
                        if v.thread.state == "open" {
                            t.ok
                        } else {
                            t.muted
                        },
                    )];
                    facts.push(
                        div()
                            .text_color(t.muted)
                            .child(format!("#{} · {}", v.thread.id, v.thread.dossier)),
                    );
                    if let Some(account) = &v.thread.account {
                        facts.push(
                            div()
                                .text_color(t.muted)
                                .child(format!("account {account}")),
                        );
                    }
                    (
                        v.thread.title.clone(),
                        facts,
                        v.members
                            .iter()
                            .filter(|m| m.state != "left")
                            .map(|m| m.name.clone())
                            .collect(),
                    )
                }
                (None, None) => ("No open thread".into(), Vec::new(), Vec::new()),
            };
        div()
            .v_flex()
            .gap(S2)
            .p(S3)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .h_flex()
                    .gap(S2)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(T_MD)
                            .font_semibold()
                            .text_color(t.text)
                            .child(title),
                    )
                    .child(fold("fold-2", 1, t, cx)),
            )
            .child(
                div()
                    .h_flex()
                    .flex_wrap()
                    .gap(S2)
                    .text_size(T_XS)
                    .children(facts),
            )
            .when(!members.is_empty(), |d| {
                d.child(
                    div()
                        .h_flex()
                        .flex_wrap()
                        .gap(S1)
                        .children(members.iter().map(|m| {
                            div()
                                .h_flex()
                                .gap(S1)
                                .pl(px(2.))
                                .pr(S2)
                                .py(px(2.))
                                .rounded_full()
                                .bg(t.card)
                                .text_size(T_XS)
                                .text_color(t.text2)
                                .child(avatar(m, 18.))
                                .child(m.clone())
                        })),
                )
            })
    }

    /// Pane 2: the thread as a timeline, then this screen's runs.
    fn pane2(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        if self.screen.folded[1] {
            return rail("pane-2", "thread", t, cx, 1);
        }
        let now = unix_now();
        let rows: Vec<AnyElement> = self
            .screen
            .thread
            .iter()
            .flat_map(|v| v.log.iter())
            .map(|r| log_row(r, now, t))
            .collect();
        let empty = rows.is_empty();
        let runs = (!self.screen.runs.is_empty()).then(|| {
            div()
                .v_flex()
                .gap(S1)
                .pt(S3)
                .child(
                    div()
                        .text_size(T_XS)
                        .font_semibold()
                        .text_color(t.muted)
                        .child("FROM THIS SCREEN"),
                )
                .children(self.screen.runs.iter().map(|(line, ok)| {
                    let (glyph, color) = match ok {
                        None => (IconName::LoaderCircle, t.muted),
                        Some(true) => (IconName::CircleCheck, t.ok),
                        Some(false) => (IconName::CircleX, t.critical),
                    };
                    div()
                        .h_flex()
                        .gap(S2)
                        .text_size(T_SM)
                        .text_color(t.text2)
                        .child(icon(glyph, px(14.), color))
                        .child(div().truncate().child(line.replace('\n', " ↵ ")))
                }))
        });
        div()
            .id("pane-2")
            .w(relative(0.30))
            .flex_none()
            .h_full()
            .v_flex()
            .bg(t.pane)
            .border_r_1()
            .border_color(t.border)
            .child(self.thread_header(t, cx))
            .child(
                div()
                    .id("events")
                    .flex_1()
                    .overflow_y_scroll()
                    .v_flex()
                    .px(S3)
                    .pb(S4)
                    .children(rows)
                    .when(empty && self.screen.thread_error.is_none(), |d| {
                        d.child(
                            div()
                                .pt(S3)
                                .text_size(T_SM)
                                .text_color(t.muted)
                                .child("Nothing in the thread yet"),
                        )
                    })
                    .children(runs),
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

    /// The chat bar: the text or its placeholder, raised over the page.
    fn chat_bar(
        &self,
        width: gpui_kit::DefiniteLength,
        t: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let focused = self.chat_focus.is_focused(window);
        let text: Vec<Div> = if self.screen.chat.is_empty() {
            vec![div().text_color(t.muted).child(if focused {
                "▌Ask, or type / for commands"
            } else {
                "Ask, or type / for commands"
            })]
        } else {
            let mut text = self.screen.chat.clone();
            if focused {
                text.push('▌');
            }
            text.split('\n')
                .map(|l| div().child(l.to_string()))
                .collect()
        };
        div()
            .id("chat")
            .track_focus(&self.chat_focus)
            .w(width)
            .max_h(relative(0.4))
            .overflow_hidden()
            .h_flex()
            .items_end()
            .gap(S3)
            .px(S4)
            .py(S3)
            .rounded(R_LG)
            .border_1()
            .border_color(if focused {
                t.accent.opacity(0.6)
            } else {
                t.border
            })
            .bg(t.raised)
            .shadow_lg()
            .cursor_text()
            .child(
                div()
                    .pb(px(2.))
                    .child(icon(IconName::MessageSquare, px(16.), t.muted)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .v_flex()
                    .justify_end()
                    .text_size(T_SM)
                    .text_color(t.text)
                    .children(text),
            )
            .child(div().text_size(T_XS).text_color(t.muted).child("Enter ↵"))
            .on_click(cx.listener(|app, _, window, cx| app.chat_focus.focus(window, cx)))
            .test_support()
    }

    /// Pane 3 with the chat bar at its bottom centre and the completion list above the bar.
    fn pane3(&self, body: Div, t: &Tokens, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let width =
            relative(f32::from(u16::try_from(self.screen.chat_width).unwrap_or(100)) / 100.);
        let bar = self.chat_bar(width, t, window, cx);
        let entries = self.screen.completions();
        let list = (!entries.is_empty()).then(|| {
            div()
                .id("completions")
                .w(width)
                .v_flex()
                .p(S1)
                .rounded(R_MD)
                .border_1()
                .border_color(t.border)
                .bg(t.raised)
                .shadow_lg()
                .text_size(T_XS)
                .children(entries.iter().take(12).enumerate().map(|(i, e)| {
                    div()
                        .id(SharedString::from(format!("completion-{i}")))
                        .h_flex()
                        .gap(S3)
                        .px(S2)
                        .py(S1)
                        .rounded(R_SM)
                        .when(i == 0, |d| d.bg(t.accent.opacity(0.12)))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(if e.agent { t.accent } else { t.text })
                                .child(e.text.clone()),
                        )
                        .child(div().text_color(t.muted).truncate().child(e.about.clone()))
                        .test_support()
                        .into_any_element()
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
            .bg(t.window)
            .child(
                div()
                    .id("body")
                    .size_full()
                    .overflow_y_scroll()
                    .v_flex()
                    .px(S5)
                    .pt(S5)
                    // the chat bar and the completions float over the end of the page
                    .pb(px(180.))
                    .child(body),
            )
            .child(
                div()
                    .absolute()
                    .bottom(S4)
                    .left_0()
                    .right_0()
                    .top_0()
                    .v_flex()
                    .justify_end()
                    .items_center()
                    .gap(S1)
                    .children(list)
                    .child(bar),
            )
            .test_support()
            .into_any_element()
    }

    /// The three panes.
    pub(crate) fn panes(
        &self,
        body: Div,
        t: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .h_flex()
            .child(self.pane1(t, cx))
            .child(self.pane2(t, cx))
            .child(self.pane3(body, t, window, cx))
    }
}

/// A view's widget, or the error in its frame (C6).
fn widget(data: &ViewData, cx: &Context<LegionApp>) -> Div {
    let t = Tokens::of(cx);
    let frame = card(&t).child(
        div()
            .text_size(T_MD)
            .font_semibold()
            .child(data.name.clone()),
    );
    if let Some(error) = &data.error {
        return frame.child(
            div()
                .text_sm()
                .text_color(t.critical)
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
                    line.text_color(t.ok)
                } else if l.starts_with('-') {
                    line.text_color(t.critical)
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
    let t = Tokens::of(cx);
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
            div().h(px(8.)).w_full().rounded_md().bg(t.raised).child(
                div()
                    .h_full()
                    .w(relative(fraction))
                    .rounded_md()
                    .bg(t.accent),
            ),
        )
        .child(div().text_xs().child(format!("{value} / {}", data.max)))
        .test_support()
        .into_any_element()
}

/// One row of a table: a cell of equal width for each value.
fn cells(values: &[String], cx: &Context<LegionApp>) -> Div {
    let t = Tokens::of(cx);
    let n = u16::try_from(values.len().max(1)).unwrap_or(u16::MAX);
    div()
        .h_flex()
        .border_b_1()
        .border_color(t.border)
        .children(values.iter().map(|v| {
            div()
                .w(relative(1. / f32::from(n)))
                .truncate()
                .child(v.clone())
        }))
}
