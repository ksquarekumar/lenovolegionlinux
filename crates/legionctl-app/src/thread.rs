//! The tabs' content and their chat bar (design 23 P8, DESIGN-09): a thread tab shows its timeline, members and
//! rules; a view tab shows its widget; each tab has its chat bar below its content (it never covers it), 60% of the
//! tab's width at the start, wider and narrower with Ctrl+Right and Ctrl+Left, taller with its text up to 40% of the
//! tab. The `/` commands and the prompts are the TUI's (`legionctl_api::chat`). The screen reads the threads and the
//! views again at each event of the operator's event stream, not on a timer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AsyncApp, Context, Div, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    WeakEntity, Window, div, px, relative,
};
use legionctl_api::chat::{self, Action, Entry};
use legionctl_api::{
    AgentCommand, ClientPath, EventRow, ProjectIn, RunOutput, SessionIdIn, ThreadIdIn, ThreadInfo,
    ThreadLogRow, ThreadView, ViewData, ViewRenderIn, op,
};
use serde_json::{Value, json};

use crate::LegionApp;
use crate::activity::Activity;
use crate::shell::{Drag, Space, Tab};
use crate::theme::{
    R_LG, R_MD, R_SM, S1, S2, S3, S4, S5, T_MD, T_SM, T_XS, Tokens, badge, card, icon,
};

/// What the screen knows; the operator holds the rest (C6: a restart reads it again).
#[derive(Debug)]
pub struct Screen {
    pub space: Space,
    /// The navigator's width: a part of the window.
    pub navigator: f32,
    pub navigator_open: bool,
    pub tabs: Vec<Tab>,
    pub active: usize,
    /// Tabs opened before the layout facts came.
    pub early: Vec<Tab>,
    /// The layout facts were read: changes are kept from now on.
    pub restored: bool,
    pub drag: Option<Drag>,
    /// The chat bar's text of each tab (by its key).
    pub chats: BTreeMap<String, String>,
    /// The chat bar's width in percent of the tab.
    pub chat_width: u32,
    pub threads: Vec<ThreadInfo>,
    /// The threads that tabs show, by id.
    pub shown: BTreeMap<i64, ThreadView>,
    /// The chat bar's thread: the thread of the tab, else the newest open one.
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
    /// The data of each view tab (by its key), and when it was read.
    pub view_data: BTreeMap<String, ViewData>,
    pub rendered_at: BTreeMap<String, std::time::Instant>,
    /// The views of `views.list`: the navigator's entries.
    pub views: Vec<legionctl_api::ViewInfo>,
    /// The groups whose fold the user changed (`<view>:<group>`).
    pub toggled: BTreeSet<String>,
    /// The graph nodes that are open (`<view>:<node>`), and the children read for them.
    pub opened: BTreeSet<String>,
    pub lazy: BTreeMap<String, Result<Vec<legionctl_api::ViewNode>, String>>,
    /// The log id of the newest `views.place` that this screen applied, and whether it read the threads once.
    pub placed: i64,
    pub seen: bool,
    /// The events that this screen got from the stream.
    pub events: usize,
    /// The log rows whose full text is open (U5).
    pub expanded: BTreeSet<i64>,
    pub activity: Activity,
    /// When the operator last answered the glance values.
    pub updated: Option<std::time::Instant>,
    /// The refreshes since the start (the Activity reads its slow parts each 15th).
    pub ticks: u32,
}

impl Screen {
    #[must_use]
    pub fn new(repo: PathBuf) -> Screen {
        Screen {
            space: Space::Machine,
            navigator: crate::shell::NAVIGATOR,
            navigator_open: true,
            tabs: Vec::new(),
            early: Vec::new(),
            active: 0,
            restored: false,
            drag: None,
            chats: BTreeMap::new(),
            chat_width: 60,
            threads: Vec::new(),
            shown: BTreeMap::new(),
            thread: None,
            thread_error: None,
            runs: Vec::new(),
            commands: Vec::new(),
            attached: None,
            agents: Vec::new(),
            default_agent: None,
            dossier: None,
            repo,
            view_data: BTreeMap::new(),
            rendered_at: BTreeMap::new(),
            views: Vec::new(),
            toggled: BTreeSet::new(),
            opened: BTreeSet::new(),
            lazy: BTreeMap::new(),
            placed: 0,
            seen: false,
            events: 0,
            expanded: BTreeSet::new(),
            activity: Activity::default(),
            updated: None,
            ticks: 0,
        }
    }

    fn key(&self) -> String {
        self.tabs.get(self.active).map(Tab::key).unwrap_or_default()
    }

    /// The chat bar's text of the tab that shows.
    #[must_use]
    pub fn chat(&self) -> &str {
        self.chats.get(&self.key()).map_or("", String::as_str)
    }

    pub fn chat_mut(&mut self) -> &mut String {
        let key = self.key();
        self.chats.entry(key).or_default()
    }

    /// The data of the view tab that shows.
    #[must_use]
    pub fn view(&self) -> Option<&ViewData> {
        self.view_data.get(&self.key())
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
        let screen = self
            .views
            .iter()
            .map(|v| {
                (
                    format!("/view {}", v.name),
                    format!("Open {} in a tab", v.title.as_deref().unwrap_or(&v.name)),
                )
            })
            .chain(self.agents.iter().map(|a| {
                let current = if now.as_ref() == Some(a) { ", now" } else { "" };
                (
                    format!("/agent {a}"),
                    format!("Send the prompts to {a} (session.acp{current})"),
                )
            }))
            .collect();
        chat::completions(self.chat(), chat::entries(&self.commands, screen))
    }
}

impl LegionApp {
    /// Read the threads, the thread of each thread tab, the chat bar's thread (the tab's, else the newest open one)
    /// and its agents (config `session.acp`, `app.agent`), and apply a new placement of a view.
    pub(crate) fn read_thread(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let project = ProjectIn {
            project: ClientPath(self.screen.repo.clone()),
        };
        let mut wanted: BTreeSet<i64> = self
            .screen
            .tabs
            .iter()
            .filter_map(|t| match t {
                Tab::Thread { id } => Some(*id),
                Tab::View { .. } => None,
            })
            .collect();
        let current = match self.tab() {
            Some(Tab::Thread { id }) => Some(*id),
            _ => None,
        };
        let task = self.operator(cx, async move {
            let config = client.call::<op::ConfigShow>(&project).await?.value;
            let threads = client
                .call::<op::ThreadList>(&legionctl_api::Empty {})
                .await?;
            let chat = current.or_else(|| threads.iter().find(|t| t.state == "open").map(|t| t.id));
            wanted.extend(chat);
            let mut shown = BTreeMap::new();
            for id in wanted {
                shown.insert(
                    id,
                    client
                        .call::<op::ThreadShow>(&ThreadIdIn { thread: id })
                        .await?,
                );
            }
            Ok((threads, shown, chat, config))
        });
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| {
                    match result {
                        Ok((threads, shown, chat, config)) => {
                            app.screen.agents = config["session"]["acp"]
                                .as_object()
                                .map(|acp| acp.keys().cloned().collect())
                                .unwrap_or_default();
                            app.screen.default_agent =
                                config["app"]["agent"].as_str().map(String::from);
                            app.screen.threads = threads;
                            app.screen.thread = chat.and_then(|id| shown.get(&id).cloned());
                            app.screen.shown = shown;
                            app.screen.thread_error = None;
                            app.apply_layout(cx);
                            app.screen.seen = true;
                        }
                        Err(e) => app.screen.thread_error = Some(format!("{e:#}")),
                    }
                    cx.notify();
                });
            },
        )
        .detach();
    }

    /// The newest `views.place` row of the threads that this screen reads opens its view as a tab, once.
    fn apply_layout(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self
            .screen
            .shown
            .values()
            .flat_map(|t| t.log.iter())
            .filter(|r| r.kind == "views.place")
            .max_by_key(|r| r.id)
            .cloned()
        else {
            return;
        };
        if row.id <= self.screen.placed {
            return;
        }
        self.screen.placed = row.id;
        // a placement before this screen's first read is history: its view opens when the user asks for it
        if !self.screen.seen {
            return;
        }
        let p = &row.payload;
        let params = p["params"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
            .collect();
        let tab = Tab::View {
            name: p["view"].as_str().unwrap_or_default().to_string(),
            params,
        };
        self.screen.view_data.remove(&tab.key());
        self.open_tab(tab, cx);
    }

    /// Read the view of the tab that shows again.
    pub(crate) fn render_view(&mut self, cx: &mut Context<Self>) {
        let Some(Tab::View { name, params }) = self.tab().cloned() else {
            return;
        };
        let key = self.tab_key();
        let client = self.client.clone();
        let repo = self.screen.repo.clone();
        let task = self.operator(cx, async move {
            client
                .call::<op::ViewsRender>(&ViewRenderIn {
                    name,
                    params,
                    repo: ClientPath(repo),
                })
                .await
        });
        self.finish(cx, task, move |app, data| {
            app.screen
                .rendered_at
                .insert(key.clone(), std::time::Instant::now());
            app.screen.view_data.insert(key, data);
        });
    }

    /// Events of the operator's stream: the threads again, the views that an event refreshes, the Activity.
    pub(crate) fn events(&mut self, rows: Vec<EventRow>, cx: &mut Context<Self>) {
        self.screen.events += rows.len();
        let refreshed: Vec<String> = self
            .screen
            .view_data
            .iter()
            .filter(|(_, v)| rows.iter().any(|r| v.refreshes_on(&r.kind)))
            .map(|(k, _)| k.clone())
            .collect();
        for key in refreshed {
            self.screen.view_data.remove(&key);
        }
        if rows
            .iter()
            .any(|r| r.kind == "views.save" || r.kind == "views.version")
        {
            self.read_views(cx);
        }
        self.screen.activity.take(rows);
        self.read_thread(cx);
        self.load_tab(cx);
    }

    /// The data of the space that shows.
    pub(crate) fn load_space(&mut self, cx: &mut Context<Self>) {
        if self.screen.space == Space::Activity {
            self.load_activity(cx);
        }
    }

    /// Run `op` with `input` as the line `line` of this screen's runs; its text goes to the message line.
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
        let line = std::mem::take(self.screen.chat_mut()).trim().to_string();
        if line.is_empty() {
            return;
        }
        match chat::action(&line, &self.screen.commands, &["view", "agent"]) {
            Action::Prompt(prompt) => self.prompt(&prompt, line, cx),
            Action::Operation(op, input) => self.start(op.name, input, line, cx),
            Action::Screen(words) => self.screen_command(&words, cx),
            Action::Refused(why) => self.state.message = Some(why),
        }
        cx.notify();
    }

    fn screen_command(&mut self, words: &[String], cx: &mut Context<Self>) {
        match words.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["view", name, ..] => {
                let tab = Tab::View {
                    name: name.to_string(),
                    params: words[2..].to_vec(),
                };
                self.open_tab(tab, cx);
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
                self.state.message = Some("/view <name> [name=value...] or /agent <name>".into());
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

    /// A key on the screen: the spaces, the navigator and the tabs anywhere, the rest in the chat bar.
    pub(crate) fn key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let k = &event.keystroke;
        let control = k.modifiers.control;
        let alt = k.modifiers.alt;
        let shift = k.modifiers.shift;
        let n = self.screen.tabs.len();
        match k.key.as_str() {
            "1" | "2" | "3" | "4" if control || alt => {
                let i = k.key.parse::<usize>().unwrap_or(1) - 1;
                self.show_space(Space::ALL[i], cx);
            }
            "w" if control => self.close_tab(self.screen.active, cx),
            "tab" if control && n > 0 => {
                let next = if shift {
                    (self.screen.active + n - 1) % n
                } else {
                    (self.screen.active + 1) % n
                };
                self.show_tab(next, cx);
            }
            "left" | "right" if control && shift && n > 0 => {
                let from = self.screen.active;
                let to = if k.key == "left" {
                    from.saturating_sub(1)
                } else {
                    (from + 1).min(n - 1)
                };
                self.move_tab(from, to, cx);
            }
            _ if self.form_focus.is_focused(window) => {
                if !self.form_key(event, cx) {
                    return;
                }
            }
            _ if !self.chat_focus.is_focused(window) => return,
            "enter" if alt || shift => self.screen.chat_mut().push('\n'),
            "j" if control => self.screen.chat_mut().push('\n'),
            "enter" => self.submit(cx),
            "right" if control => self.screen.chat_width = (self.screen.chat_width + 10).min(100),
            "left" if control => {
                self.screen.chat_width = self.screen.chat_width.saturating_sub(10).max(30);
            }
            "backspace" => {
                self.screen.chat_mut().pop();
            }
            "tab" => {
                if let Some(first) = self.screen.completions().first()
                    && first.text.starts_with(self.screen.chat())
                {
                    *self.screen.chat_mut() = format!("{} ", first.text);
                }
            }
            _ => match &k.key_char {
                Some(text) if !control && !alt => self.screen.chat_mut().push_str(text),
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
    ago_short(u64::try_from(now.saturating_sub(at)).unwrap_or(0))
}

/// `seconds` as a short age: now, 5 min, 3 h, 2 d.
pub(crate) fn ago_short(seconds: u64) -> String {
    match seconds {
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

/// A thread tab's head: its title, state, dossier and account, and its members.
fn thread_header(v: &ThreadView, t: &Tokens) -> Div {
    let members = v.members.iter().filter(|m| m.state != "left").map(|m| {
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
            .child(avatar(&m.name, 18.))
            .child(format!("{} · {}", m.name, m.archetype))
    });
    let mut facts = vec![
        badge(
            v.thread.state.clone(),
            if v.thread.state == "open" {
                t.ok
            } else {
                t.muted
            },
        ),
        div().text_color(t.muted).child(format!(
            "thread {} · dossier {}",
            v.thread.id, v.thread.dossier
        )),
    ];
    if let Some(account) = &v.thread.account {
        facts.push(
            div()
                .text_color(t.muted)
                .child(format!("account {account}")),
        );
    }
    div()
        .v_flex()
        .gap(S2)
        .child(
            div()
                .text_size(T_MD)
                .font_semibold()
                .text_color(t.text)
                .child(v.thread.title.clone()),
        )
        .child(
            div()
                .h_flex()
                .flex_wrap()
                .gap(S2)
                .text_size(T_XS)
                .children(facts),
        )
        .child(div().h_flex().flex_wrap().gap(S1).children(members))
}

impl LegionApp {
    /// A row of a thread's timeline: the member's chip and name only when the actor changes (U15), the kind, the time
    /// (UTC in the tooltip), and what the row says; a long row shows its first line with "More" (U5).
    fn log_row(
        &self,
        r: &ThreadLogRow,
        actor_changed: bool,
        now: i64,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
        let more = chat::log_more(r);
        let open = self.screen.expanded.contains(&r.id);
        let id = r.id;
        div()
            .id(SharedString::from(format!("log-{}", r.id)))
            .h_flex()
            .items_start()
            .gap(S2)
            .pt(if actor_changed { S3 } else { px(2.) })
            .child(if actor_changed {
                avatar(member, 24.)
            } else {
                div().w(px(24.)).flex_none()
            })
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .when(actor_changed, |d| {
                        d.child(
                            div()
                                .font_semibold()
                                .text_size(T_XS)
                                .text_color(t.text)
                                .child(member.to_string()),
                        )
                    })
                    .child(
                        div()
                            .h_flex()
                            .items_start()
                            .gap(S2)
                            .child(badge(kind.to_string(), kind_color(kind, t)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(T_SM)
                                    .text_color(t.text2)
                                    .child(chat::log_detail(r)),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("at-{}", r.id)))
                                    .flex_none()
                                    .text_size(T_XS)
                                    .text_color(t.muted)
                                    .child(ago(now, r.at))
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(when.clone()).build(window, cx)
                                    }),
                            ),
                    )
                    .when_some(more, |d, more| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("more-{id}")))
                                .text_size(T_XS)
                                .text_color(t.accent)
                                .cursor_pointer()
                                .child(if open { "Less" } else { "More" })
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    if !app.screen.expanded.remove(&id) {
                                        app.screen.expanded.insert(id);
                                    }
                                    cx.notify();
                                }))
                                .test_support(),
                        )
                        .when(open, |d| {
                            d.child(
                                div()
                                    .text_size(T_XS)
                                    .text_color(t.text2)
                                    .children(more.lines().map(|l| div().child(l.to_string()))),
                            )
                        })
                    }),
            )
            .test_support()
            .into_any_element()
    }

    /// A thread tab: its title and facts, its members, its rules and its timeline.
    fn thread_tab(&self, id: i64, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let Some(v) = self.screen.shown.get(&id) else {
            return div()
                .text_color(t.muted)
                .child(match &self.screen.thread_error {
                    Some(e) => format!("cannot read thread {id}: {e}"),
                    None => "reading the thread…".into(),
                });
        };
        let now = unix_now();
        let mut last = None;
        let rows: Vec<AnyElement> = v
            .log
            .iter()
            .map(|r| {
                let changed = last.as_deref() != Some(r.member.as_str());
                last = Some(r.member.clone());
                self.log_row(r, changed, now, t, cx)
            })
            .collect();
        div()
            .v_flex()
            .gap(S4)
            .child(thread_header(v, t))
            .when(!v.rules.is_empty(), |d| {
                d.child(
                    card(t)
                        .gap(S1)
                        .child(crate::theme::heading("Watch rules", t))
                        .children(v.rules.iter().map(|r| {
                            div().text_size(T_SM).text_color(t.text2).child(format!(
                                "{} watches {}: after {} calls in {}, expect {} (missed {})",
                                r.member,
                                r.watched,
                                r.every,
                                if r.path.is_empty() { "/" } else { &r.path },
                                r.expect,
                                r.misses
                            ))
                        })),
                )
            })
            .child(
                div()
                    .id("timeline")
                    .v_flex()
                    .children(rows)
                    .when(v.log.is_empty(), |d| {
                        d.child(div().text_color(t.muted).child("Nothing in the thread yet"))
                    })
                    .test_support(),
            )
    }

    /// The chat bar of the tab: the agent chip, the text or its placeholder.
    fn chat_bar(
        &self,
        width: gpui_kit::DefiniteLength,
        t: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let focused = self.chat_focus.is_focused(window);
        let chat = self.screen.chat();
        let text: Vec<Div> = if chat.is_empty() {
            vec![div().text_color(t.muted).child(if focused {
                "▌Ask, or type / for commands"
            } else {
                "Ask, or type / for commands"
            })]
        } else {
            let mut text = chat.to_string();
            if focused {
                text.push('▌');
            }
            text.split('\n')
                .map(|l| div().child(l.to_string()))
                .collect()
        };
        let agent = self.screen.agent();
        let chip = div()
            .id("agent-chip")
            .flex_none()
            .h_flex()
            .gap(S1)
            .px(S2)
            .py(px(2.))
            .rounded_full()
            .bg(if agent.is_some() {
                t.accent.opacity(0.14)
            } else {
                t.warn.opacity(0.16)
            })
            .text_size(T_XS)
            .text_color(if agent.is_some() { t.accent } else { t.warn })
            .cursor_pointer()
            .child(icon(
                IconName::Bot,
                px(12.),
                if agent.is_some() { t.accent } else { t.warn },
            ))
            .child(agent.clone().unwrap_or_else(|| "pick an agent".into()))
            .tooltip(|window, cx| {
                Tooltip::new("The agent of this chat bar: /agent <name>").build(window, cx)
            })
            .on_click(cx.listener(|app, _, window, cx| {
                *app.screen.chat_mut() = "/agent ".into();
                app.chat_focus.focus(window, cx);
                cx.stop_propagation();
                cx.notify();
            }))
            .test_support();
        div()
            .id("chat")
            .track_focus(&self.chat_focus)
            .w(width)
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
            .cursor_text()
            .child(chip)
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
            .child(
                div()
                    .flex_none()
                    .text_size(T_XS)
                    .text_color(t.muted)
                    .child("Enter ↵"),
            )
            .on_click(cx.listener(|app, _, window, cx| app.chat_focus.focus(window, cx)))
            .test_support()
    }

    /// The tab's body: its content (scrolls), then its chat bar with the completion list above it, at most 40% of
    /// the tab. The bar is below the content, so it never covers a row (U2).
    pub(crate) fn tab_body(
        &self,
        content: Div,
        t: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let width =
            relative(f32::from(u16::try_from(self.screen.chat_width).unwrap_or(100)) / 100.);
        let entries = self.screen.completions();
        let list = (!entries.is_empty()).then(|| {
            div()
                .id("completions")
                .w(width)
                .max_h(px(260.))
                .overflow_y_scroll()
                .v_flex()
                .p(S1)
                .rounded(R_MD)
                .border_1()
                .border_color(t.border)
                .bg(t.raised)
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
            .id("tab")
            .flex_1()
            .min_h_0()
            .v_flex()
            .child(
                div()
                    .id("body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.body_scroll)
                    .px(S5)
                    .pt(S5)
                    .pb(S4)
                    // the content keeps a readable width on wide windows (U10)
                    .child(div().max_w(px(1180.)).child(content))
                    .test_support(),
            )
            .child(
                div()
                    .id("chat-area")
                    .flex_none()
                    .max_h(relative(0.4))
                    .v_flex()
                    .items_center()
                    .gap(S1)
                    .px(S5)
                    .pb(S3)
                    .pt(S1)
                    .border_t_1()
                    .border_color(t.border)
                    .children(list)
                    .child(self.chat_bar(width, t, window, cx))
                    .test_support(),
            )
            .test_support()
            .into_any_element()
    }

    /// The content of the tab that shows.
    pub(crate) fn tab_content(&self, t: &Tokens, width: f32, cx: &mut Context<Self>) -> Div {
        match self.tab().cloned() {
            None => div()
                .text_color(t.muted)
                .child("No tab: pick an entry in the navigator, or type an intent below"),
            Some(Tab::Thread { id }) => self.thread_tab(id, t, cx),
            Some(Tab::View { .. }) => match self.screen.view() {
                Some(data) => div().child(self.view_tab(data, width, t, cx)),
                None => div().text_color(t.muted).child("reading the view…"),
            },
        }
    }
}
