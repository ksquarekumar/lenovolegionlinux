//! The tabs' content and their chat bar (design 23 P8, DESIGN-09): a thread tab shows its timeline, members and
//! rules; a view tab shows its widget; each tab has its chat bar below its content (it never covers it), 60% of the
//! tab's width at the start, wider and narrower with Ctrl+Right and Ctrl+Left, taller with its text up to 40% of the
//! tab. The `/` commands and the prompts come from `legionctl_api::chat`. The screen reads the threads and the
//! views again at each event of the operator's event stream, not on a timer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Sizable as _, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AsyncApp, Context, Div, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    WeakEntity, Window, div, px, relative,
};
use legionctl_api::chat::{self, Action, Entry};
use legionctl_api::{
    AgentCommand, EventRow, RunOutput, SessionIdIn, ThreadIdIn, ThreadInfo, ThreadLogRow,
    ThreadView, ViewAnswer, ViewData, ViewRenderIn, op,
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
    /// The chat bar's input history.
    pub history: crate::history::History,
    /// The chat bar's list of commands: its pick, and whether Esc closed it.
    pub list: List,
    pub threads: Vec<ThreadInfo>,
    /// The threads that tabs show, by id.
    pub shown: BTreeMap<i64, ThreadView>,
    /// The chat bar's thread: the thread of the tab (a chat tab's own thread); none in a view tab or a new chat.
    pub thread: Option<ThreadView>,
    pub thread_error: Option<String>,
    /// This screen's lines that ran, each with its result once it ended.
    pub runs: Vec<(String, Option<bool>)>,
    /// The attached agent's commands (from its last run).
    pub commands: Vec<AgentCommand>,
    /// The agents of config `session.acp`, and `app.agent`: the chat bar's agent where the thread has no pick.
    pub agents: Vec<String>,
    pub default_agent: Option<String>,
    /// The user's dossier (`dossier use` with no session: the fact `app` `dossier`): new chats and the composer's
    /// runs work in it; none: the app asks the user to choose or make one. The dossiers there are, newest first.
    pub dossier: Option<String>,
    pub dossiers: Vec<String>,
    /// The screen's newest `/agent` pick: a new chat and a view tab's run take it (a thread keeps its own pick).
    pub draft_agent: Option<String>,
    /// A new chat works in the app's project from this base ref (empty: the current branch); none: in a new folder
    /// of its own. The user's choice of the project sets it.
    pub chat_place: Option<String>,
    /// The answer of a thread's run as it comes (`session.output` of its tag), by thread: (tag, text so far).
    pub live: BTreeMap<i64, (String, String)>,
    /// The app's project (DESIGN-20 rule 7): the folder of every call (its config layer, views, code and facts) and
    /// the folder of new chats; the start folder until the user chooses one (the fact `app` `project`).
    pub project: PathBuf,
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
    /// The accounts (the current login and clauth's other profiles) and their usage (`agents accounts list`,
    /// `agents accounts usage`, todo-136).
    pub accounts: Vec<legionctl_api::Account>,
    pub usage: Vec<legionctl_api::AccountUsage>,
    /// Whose account picker shows: a member's name, or "" for the chat bar.
    pub picking: Option<String>,
}

impl Screen {
    #[must_use]
    pub fn new(project: PathBuf) -> Screen {
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
            history: crate::history::History::default(),
            list: List::default(),
            threads: Vec::new(),
            shown: BTreeMap::new(),
            thread: None,
            thread_error: None,
            runs: Vec::new(),
            commands: Vec::new(),
            agents: Vec::new(),
            default_agent: None,
            dossier: None,
            dossiers: Vec::new(),
            draft_agent: None,
            chat_place: None,
            live: BTreeMap::new(),
            project,
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
            accounts: Vec::new(),
            usage: Vec::new(),
            picking: None,
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

    /// The chat bar's agent: in a thread's tab the thread's newest `/agent` pick (a `thread.agent` row); in another
    /// tab the screen's newest `/agent` pick; else `app.agent`.
    #[must_use]
    pub fn agent(&self) -> Option<String> {
        match self.tab_thread() {
            Some(t) => t
                .log
                .iter()
                .rev()
                .find(|r| r.kind == "thread.agent")
                .and_then(|r| r.payload["agent"].as_str().map(String::from)),
            None => self.draft_agent.clone(),
        }
        .or_else(|| self.default_agent.clone())
    }

    /// The active tab's thread; not `thread`, which changes only when a new tab's thread is read.
    fn tab_thread(&self) -> Option<&ThreadView> {
        match self.tabs.get(self.active) {
            Some(Tab::Thread { id }) => self.shown.get(id),
            _ => None,
        }
    }

    /// The chat bar's account: the thread's newest chat bar pick (a `thread.account` row of no member; none: the
    /// current login), else the thread's account.
    #[must_use]
    pub fn chat_account(&self) -> Option<String> {
        let thread = self.tab_thread()?;
        match thread
            .log
            .iter()
            .rev()
            .find(|r| r.kind == "thread.account" && r.member.is_empty())
        {
            Some(pick) => pick.payload["account"].as_str().map(String::from),
            None => thread.thread.account.clone(),
        }
    }

    /// The use rows of an account (none: the current login's).
    fn usage_rows(
        &self,
        account: Option<&str>,
    ) -> impl Iterator<Item = &legionctl_api::AccountUsage> + '_ {
        let account = account.map(String::from);
        self.usage.iter().filter(move |u| match &account {
            None => u.source == "current",
            Some(a) => u.account == *a && u.source != "current",
        })
    }

    /// clauth's state of an account (none: the current login), each agent's sentence on a line: the chip's tooltip.
    #[must_use]
    pub fn account_state(&self, account: Option<&str>) -> String {
        self.usage_rows(account)
            .map(|u| u.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// An account's use in a few words (`Claude 5h 42% 7d 8%`, `Codex use unknown`), each agent's; none: the
    /// current login's.
    #[must_use]
    pub fn usage_of(&self, account: Option<&str>) -> String {
        self.usage_rows(account)
            .map(|u| {
                let agent = if u.agent == "claude" {
                    "Claude"
                } else {
                    "Codex"
                };
                match &u.error {
                    Some(_) => format!("{agent} use unknown"),
                    None => std::iter::once(agent.to_string())
                        .chain(
                            u.windows
                                .iter()
                                .map(|w| format!("{} {:.0}%", w.name, w.used_percent)),
                        )
                        .collect::<Vec<_>>()
                        .join(" "),
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// The step that gets an account's use read (Start clauth; none: the current login's), once.
    #[must_use]
    pub fn usage_fix(&self, account: Option<&str>) -> Option<legionctl_api::ResourceAction> {
        self.usage_rows(account)
            .find_map(|u| u.actions.first())
            .cloned()
    }

    /// The `/` commands that match the chat bar's text: the agent's first; none after Esc.
    #[must_use]
    pub fn completions(&self) -> Vec<Entry> {
        if self.list.closed {
            return Vec::new();
        }
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
            .chain(std::iter::once((
                "/feedback ".to_string(),
                "Write a note about this tab, with a screenshot".to_string(),
            )))
            .chain(self.agents.iter().map(|a| {
                let current = if now.as_ref() == Some(a) {
                    " (now)"
                } else {
                    ""
                };
                (
                    format!("/agent {a}"),
                    format!("Send your messages to {a}{current}"),
                )
            }))
            .chain(std::iter::once((
                "/chat".to_string(),
                "Open a new chat with the agent".to_string(),
            )))
            .chain(self.dossiers.iter().map(|d| {
                let current = if self.dossier.as_ref() == Some(d) {
                    " (now)"
                } else {
                    ""
                };
                (
                    format!("/dossier use {d}"),
                    format!("Use dossier {d}{current}"),
                )
            }))
            .chain([
                (
                    "/dossier create ".to_string(),
                    "Make a dossier and use it".to_string(),
                ),
                ("/dossier list".to_string(), "List the dossiers".to_string()),
                ("/dossier release".to_string(), "Use no dossier".to_string()),
            ])
            .collect();
        let mut entries = chat::entries(&self.commands, screen);
        entries.retain(|e| !agents_only(&e.text));
        chat::completions(self.chat(), entries)
    }
}

/// The chat's line of where it works: the folder of its runs, which the agent names too.
#[must_use]
pub fn place_line(th: &ThreadInfo) -> String {
    let Some(folder) = &th.folder else {
        return "Works in a new folder of this chat".into();
    };
    match (&th.repo, &th.branch) {
        (Some(repo), Some(branch)) => format!("Works in {folder}, on branch {branch} of {repo}"),
        (Some(repo), None) => {
            format!("Works in {folder}, a copy of {repo}: its changes stay in the copy")
        }
        _ => format!("Works in {folder}, a new folder of this chat"),
    }
}

impl LegionApp {
    /// Read the threads, the thread of each thread tab, the chat bar's thread (the tab's, else the newest open one)
    /// and its agents (config `session.acp`, `app.agent`), and apply a new placement of a view.
    pub(crate) fn read_thread(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let wanted: BTreeSet<i64> = self
            .screen
            .tabs
            .iter()
            .filter_map(|t| match t {
                Tab::Thread { id } => Some(*id),
                Tab::View { .. } | Tab::Chat => None,
            })
            .collect();
        let chat = match self.tab() {
            Some(Tab::Thread { id }) => Some(*id),
            _ => None,
        };
        let task = self.operator(cx, async move {
            let config = client
                .call::<op::ConfigShow>(&legionctl_api::Empty {})
                .await?
                .value;
            let threads = client
                .call::<op::ThreadList>(&legionctl_api::Empty {})
                .await?;
            let dossiers = client
                .call::<op::DossierListOp>(&legionctl_api::Empty {})
                .await?;
            let facts = client
                .call::<op::FactsAt>(&legionctl_api::FactsAtIn {
                    entity: Some("app".into()),
                    dossier: None,
                    thread: None,
                    time: None,
                    recorded: None,
                    rev: None,
                    winners: true,
                })
                .await?;
            let fact = |name: &str| {
                facts
                    .iter()
                    .find(|f| f.attribute == name)
                    .and_then(|f| f.value.as_str().map(String::from))
            };
            let (dossier, project) = (fact("dossier"), fact("project"));
            let mut shown = BTreeMap::new();
            for id in wanted {
                shown.insert(
                    id,
                    client
                        .call::<op::ThreadShow>(&ThreadIdIn { thread: id })
                        .await?,
                );
            }
            let dossiers: Vec<String> = dossiers.into_iter().map(|d| d.name).collect();
            Ok((threads, shown, chat, config, (dossier, dossiers, project)))
        });
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| {
                    match result {
                        Ok((threads, shown, chat, config, (dossier, dossiers, project))) => {
                            app.screen.dossier = dossier;
                            // the project chosen before, while it is still a folder
                            if let Some(p) = project.map(PathBuf::from).filter(|p| p.is_dir())
                                && p != app.screen.project
                            {
                                app.use_project(&p);
                            }
                            app.screen.dossiers = dossiers;
                            app.screen.agents = config["session"]["acp"]
                                .as_object()
                                .map(|acp| acp.keys().cloned().collect())
                                .unwrap_or_default();
                            app.screen.default_agent =
                                config["app"]["agent"].as_str().map(String::from);
                            app.screen.threads = threads;
                            let rows = |t: &Option<ThreadView>| {
                                t.as_ref().map(|v| (v.thread.id, v.log.len()))
                            };
                            let before = rows(&app.screen.thread);
                            app.screen.thread = chat.and_then(|id| shown.get(&id).cloned());
                            // a chat shows its newest message: a new row (or a thread just opened) scrolls down
                            if rows(&app.screen.thread).is_some_and(|now| Some(now) != before) {
                                app.follow_chat();
                            }
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

    /// Read the Dossiers view for the new chat's dossier picker.
    pub(crate) fn load_picker(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            client
                .call::<op::ViewsRender>(&ViewRenderIn {
                    name: "dossiers".into(),
                    params: Vec::new(),
                })
                .await
        });
        self.finish(cx, task, |app, data| {
            app.screen.view_data.insert(PICKER.into(), data);
        });
    }

    /// Read the view of the tab that shows again.
    pub(crate) fn render_view(&mut self, cx: &mut Context<Self>) {
        let Some(Tab::View { name, params }) = self.tab().cloned() else {
            return;
        };
        let key = self.tab_key();
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            client
                .call::<op::ViewsRender>(&ViewRenderIn { name, params })
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
        // a chat's answer as it comes: the output of its run's tag
        for row in &rows {
            if row.kind != "session.output" {
                continue;
            }
            let (Some(tag), Some(text)) =
                (row.payload["tag"].as_str(), row.payload["text"].as_str())
            else {
                continue;
            };
            let active = match self.tab() {
                Some(Tab::Thread { id }) => Some(*id),
                _ => None,
            };
            for (id, (live_tag, so_far)) in &mut self.screen.live {
                if live_tag == tag {
                    so_far.push_str(text);
                    if active == Some(*id)
                        && self.state.form.is_none()
                        && self.state.pending.is_none()
                    {
                        self.body_scroll.scroll_to_bottom();
                    }
                }
            }
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
        let sent = input.clone();
        let task = self.operator(cx, async move { client.call_value(op, &input).await });
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| {
                    let (text, ok) = match result {
                        Ok(value) => {
                            app.after(op, &sent, cx);
                            if op == "session.run"
                                && let Ok(run) = serde_json::from_value::<RunOutput>(value.clone())
                            {
                                app.attach(&run, cx);
                            }
                            if op == "ask"
                                && let Ok(answer) =
                                    serde_json::from_value::<ViewAnswer>(value.clone())
                            {
                                let said = answer_words(&answer);
                                app.answered(answer, cx);
                                said
                            } else {
                                legionctl_api::render(op, value)
                                    .unwrap_or_else(|e| (format!("{e:#}"), false))
                            }
                        }
                        Err(e) => (format!("{e:#}"), false),
                    };
                    // an opened view with no words of the agent is its own answer: no message
                    app.state.message = (!text.is_empty()).then_some(text);
                    app.screen.runs[row].1 = Some(ok);
                    cx.notify();
                });
            },
        )
        .detach();
    }

    /// The agent's run: its commands are offered in the chat bar.
    fn attach(&mut self, run: &RunOutput, cx: &mut Context<Self>) {
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
        self.keep_line(&line, cx);
        // a change of a dossier's files is an agent's work: the line goes to the agent as a message
        if agents_only(&line) {
            self.prompt(line.trim_start_matches('/'), line.clone(), cx);
            cx.notify();
            return;
        }
        match chat::action(
            &line,
            &self.screen.commands,
            &["view", "agent", "chat", "feedback"],
            &self.screen.project,
        ) {
            Action::Prompt(prompt) => self.prompt(&prompt, line, cx),
            // a change from the chat bar or Ctrl+K takes the same plan or confirm as its button (U-13); no form: the
            // parser has refused a missing required field, and an optional one left out keeps its default
            Action::Operation(op, input)
                if op.class != legionctl_api::Class::Read && input["dry_run"] != true =>
            {
                self.confirm(
                    legionctl_api::ViewButton {
                        label: op.summary().to_string(),
                        op: op.name.to_string(),
                        input,
                        confirm: false,
                    },
                    cx,
                );
            }
            Action::Operation(op, input) => self.start(op.name, input, line, cx),
            Action::Screen(words) if words[0] == "feedback" => {
                self.feedback(line.trim_start_matches("/feedback").trim(), cx);
            }
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
                    // the thread keeps the pick; the screen's newest pick also goes to the next new chat
                    if self.screen.agents.iter().any(|a| a == name) {
                        self.screen.draft_agent = Some(name.to_string());
                    }
                    let input = json!({"thread": t.thread.id, "agent": name});
                    self.start("thread.agent", input, format!("/agent {name}"), cx);
                }
                // a new chat or a view tab: the pick holds for this screen until a thread keeps it
                None if self.screen.agents.iter().any(|a| a == name) => {
                    self.screen.draft_agent = Some(name.to_string());
                    self.state.message = Some(format!("Your next messages go to {name}"));
                }
                None => {
                    self.state.message = Some(format!(
                        "No agent named {name}. The agents are: {}",
                        self.screen.agents.join(", ")
                    ));
                }
            },
            ["chat"] => self.open_tab(Tab::Chat, cx),
            _ => {
                self.state.message =
                    Some("/view <name> [name=value...], /agent <name> or /chat".into());
            }
        }
    }

    /// Read the accounts and their use from clauth: the member chips and the account picker show them.
    pub(crate) fn read_accounts(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            let accounts = client
                .call::<op::AgentsAccountsList>(&legionctl_api::Empty {})
                .await?;
            let usage = client
                .call::<op::AgentsAccountsUsage>(&legionctl_api::AccountUsageIn { account: None })
                .await?;
            Ok((accounts, usage))
        });
        self.finish(cx, task, |app, (accounts, usage)| {
            app.screen.accounts = accounts;
            app.screen.usage = usage;
        });
    }

    /// An intent's answer (DESIGN-09 G5): its view opens as a tab with its data (a change reads the tab again), the
    /// composer's run is the chat bar's session from now on.
    fn answered(&mut self, answer: ViewAnswer, cx: &mut Context<Self>) {
        if let Some(run) = &answer.run {
            self.attach(run, cx);
        }
        if let (Some(name), Some(data)) = (answer.view, answer.data) {
            let params = match self.tab() {
                Some(Tab::View {
                    name: shown,
                    params,
                }) if *shown == name => params.clone(),
                _ => Vec::new(),
            };
            let tab = Tab::View { name, params };
            self.screen.view_data.insert(tab.key(), data);
            self.open_tab(tab, cx);
            self.read_views(cx);
        }
    }

    /// `prompt` to the agent: every line that is not a command is a message (user 2026-10-17: one input, one
    /// meaning). In a chat tab it goes to its thread; in a new chat tab it starts the thread; in a view tab the
    /// ui-composer gets it with the view: it makes the view's next version, or answers in words (an agent's own
    /// command starts a chat).
    fn prompt(&mut self, prompt: &str, line: String, cx: &mut Context<Self>) {
        let Some(agent) = self.screen.agent() else {
            self.state.message = Some(format!(
                "No agent for the chat. Choose one: {}",
                self.screen.agents.join(", ")
            ));
            return;
        };
        match self.tab().cloned() {
            Some(Tab::Thread { id }) => self.say(id, &agent, prompt, line, cx),
            // an agent's own command (`/agent:<name>`) is chat work: it starts a chat, whose runs have their own folder
            Some(Tab::View { .. }) if prompt.starts_with('/') => {
                self.new_chat(agent, prompt, line, cx);
            }
            Some(Tab::View { .. }) => self.change(prompt, line, cx),
            _ => self.new_chat(agent, prompt, line, cx),
        }
    }

    /// The first message of a new chat: the user's own thread on the user's dossier, with `agent` as its main agent;
    /// the tab becomes the thread's tab, then the message goes to the agent.
    fn new_chat(&mut self, agent: String, prompt: &str, line: String, cx: &mut Context<Self>) {
        let Some(dossier) = self.screen.dossier.clone() else {
            self.no_dossier(cx);
            return;
        };
        let title: String = prompt
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(60)
            .collect();
        let client = self.client.clone();
        let place = self.screen.chat_place.clone();
        let (who, words) = (agent.clone(), prompt.to_string());
        let task = self.operator(cx, async move {
            let start = legionctl_api::ThreadStartIn {
                title: title.clone(),
                dossier: Some(dossier.clone()),
                team: None,
                session: legionctl_api::SessionId::default(),
            };
            client.call::<op::ThreadStart>(&start).await?;
            // the newest thread of the user with this title on this dossier is the one just made
            let id = client
                .call::<op::ThreadList>(&legionctl_api::Empty {})
                .await?
                .into_iter()
                .find(|t| t.owner.is_none() && t.dossier == dossier && t.title == title)
                .map(|t| t.id)
                .ok_or_else(|| {
                    anyhow::anyhow!("the new chat is not in the list of chats; try again")
                })?;
            client
                .call::<op::ThreadAgent>(&legionctl_api::ThreadAgentIn {
                    thread: id,
                    agent: who,
                })
                .await?;
            // the app's project: the chat works in it (the call's folder), as the user chose it once
            if let Some(base) = place {
                client
                    .call::<op::ThreadRepo>(&legionctl_api::ThreadRepoIn {
                        thread: id,
                        base: (!base.is_empty()).then_some(base),
                    })
                    .await?;
            }
            Ok(id)
        });
        self.state.message = Some("Starting the chat…".into());
        self.finish_with(cx, task, move |app, id: i64, cx| {
            app.state.message = None;
            // the next chat works in the project too, from its current branch
            if let Some(base) = &mut app.screen.chat_place {
                base.clear();
            }
            if let Some(i) = app.screen.tabs.iter().position(|t| *t == Tab::Chat) {
                app.screen.tabs[i] = Tab::Thread { id };
                app.screen.active = i;
            }
            app.open_tab(Tab::Thread { id }, cx);
            app.say(id, &agent, &words, line, cx);
        });
    }

    /// `prompt` to `agent` in the user's thread `id`: the answer comes into the tab as it is written, and the thread's
    /// log keeps the prompt and the answer (or why the run failed).
    fn say(&mut self, id: i64, agent: &str, prompt: &str, line: String, cx: &mut Context<Self>) {
        let tag = format!("thread:{id}:{}", self.screen.runs.len());
        self.screen.live.insert(id, (tag.clone(), String::new()));
        self.screen.runs.push((line, None));
        let row = self.screen.runs.len() - 1;
        let account = self.screen.chat_account();
        let input = json!({
            "agent": agent, "prompt": prompt, "thread": id, "tag": tag,
            "account": account,
        });
        let client = self.client.clone();
        let task = self.operator_long(cx, async move {
            client
                .call::<op::SessionRun>(&serde_json::from_value(input)?)
                .await
        });
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| {
                    app.screen.live.remove(&id);
                    app.screen.runs[row].1 = Some(result.is_ok());
                    match result {
                        Ok(run) => {
                            app.state.message = None;
                            app.attach(&run, cx);
                        }
                        Err(e) => app.state.message = Some(format!("{e:#}")),
                    }
                    app.read_thread(cx);
                    cx.notify();
                });
            },
        )
        .detach();
        cx.notify();
    }

    /// Show the chat's newest message, unless a form or a confirm is open (it shows at the top of the tab).
    fn follow_chat(&self) {
        if self.state.form.is_none() && self.state.pending.is_none() {
            self.body_scroll.scroll_to_bottom();
        }
    }

    /// No dossier is chosen: say what a dossier is for; a new chat has the dossier picker, another tab opens the
    /// Dossiers view.
    pub(crate) fn no_dossier(&mut self, cx: &mut Context<Self>) {
        if self.tab() != Some(&Tab::Chat) {
            self.open_view("dossiers", cx);
        }
        self.state.message = Some("Choose a dossier first: click Use, or New dossier.".into());
    }

    /// What follows an operation that the user ran: a new dossier is the user's dossier at once; a changed choice
    /// shows in the title bar.
    pub(crate) fn after(&mut self, op: &str, input: &Value, cx: &mut Context<Self>) {
        match op {
            "dossier.create" => {
                if let Some(name) = input["name"].as_str() {
                    let line = format!("/dossier use {name}");
                    self.start("dossier.use", json!({"name": name}), line, cx);
                }
            }
            "dossier.use" | "dossier.release" => self.read_thread(cx),
            _ => {}
        }
    }

    /// `words` for the view tab's view: the ui-composer makes its next version (undo goes back), or answers in words.
    fn change(&mut self, words: &str, line: String, cx: &mut Context<Self>) {
        if self.screen.dossier.is_none() {
            self.no_dossier(cx);
            return;
        }
        let Some(Tab::View { name, params }) = self.tab().cloned() else {
            return;
        };
        // no ACP session to go on in: the chat bar's last run may be a chat's, whose session is no composer's
        let input = json!({
            "intent": words, "view": name, "params": params, "agent": self.screen.agent(),
            "dossier": self.screen.dossier,
            "account": self.screen.chat_account(),
        });
        self.start("ask", input, line, cx);
    }

    /// A key that no control took: the spaces and the tabs anywhere; Esc closes the list of commands, the form or
    /// the pending change; in the tab's body the keys scroll it.
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
            "k" if control => self.palette(window, cx),
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
            "right" if control => self.screen.chat_width = (self.screen.chat_width + 10).min(100),
            "left" if control => {
                self.screen.chat_width = self.screen.chat_width.saturating_sub(10).max(30);
            }
            "escape" if !self.screen.completions().is_empty() => self.screen.list.closed = true,
            "escape" if self.state.form.is_some() => self.cancel_form(cx),
            "escape" if self.state.pending.is_some() => self.discard(cx),
            // the safe choice is the default: Enter on the pending change's card discards it
            "enter" | "space" if self.pending_focus.is_focused(window) => self.discard(cx),
            "pageup" | "pagedown" => self.page(k.key == "pageup"),
            "home" | "end" => {
                self.scroll_body(px(if k.key == "home" { f32::MAX } else { f32::MIN }));
            }
            "up" | "down" if self.body_focus.is_focused(window) => {
                self.scroll_body(px(if k.key == "up" { LINE } else { -LINE }));
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Page up or page down: the tab's body scrolls by most of its height.
    pub(crate) fn page(&mut self, up: bool) {
        let page = self.body_scroll.bounds().size.height * 0.9;
        self.scroll_body(if up { page } else { -page });
    }
}

/// What the agent that makes views said, for the user: its words (none when a view opened: the view is the answer); a
/// view that fails its check says why (the route, the tokens and the data are the CLI's text, not the app's).
fn answer_words(answer: &ViewAnswer) -> (String, bool) {
    let words = answer.text.trim();
    match (&answer.error, &answer.view) {
        (Some(error), _) => (
            format!("The agent's view is not valid, so nothing changed: {error}"),
            false,
        ),
        (None, _) if !words.is_empty() => (words.to_string(), true),
        (None, Some(_)) => (String::new(), true),
        (None, None) => ("The agent gave no answer.".into(), false),
    }
}

/// A `/dossier` line that changes a dossier's files: an agent's work, not an operation the app runs.
fn agents_only(line: &str) -> bool {
    let mut words = line.split_whitespace();
    matches!(
        (words.next(), words.next()),
        (Some("/dossier"), Some("add" | "write" | "append" | "edit"))
    )
}

/// The chat bar's list of commands: the entry that Up or Down picked (Enter or Tab takes it), and whether Esc closed
/// the list (the next change of the text opens it again).
#[derive(Debug, Default)]
pub struct List {
    pub pick: Option<usize>,
    pub closed: bool,
}

/// One line of a keyboard scroll.
const LINE: f32 = 40.;

/// The commands that the chat bar's list shows at most.
pub(crate) const SHOWN: usize = 12;

/// The key of the Dossiers view that a new chat shows when the user has no dossier: choose one or make one there.
pub(crate) const PICKER: &str = "view:dossiers:";

impl LegionApp {
    /// Scroll the tab's body by `by` (up is positive), within its content.
    fn scroll_body(&mut self, by: gpui_kit::Pixels) {
        let max = self.body_scroll.max_offset().y;
        let now = self.body_scroll.offset();
        let y = (now.y + by).clamp(-max, px(0.));
        self.body_scroll.set_offset(gpui_kit::point(now.x, y));
    }

    /// How far the tab's body is scrolled down, and how far it can go (a test reads them).
    #[must_use]
    pub fn body_scrolled(&self) -> (f32, f32) {
        (
            f32::from(-self.body_scroll.offset().y),
            f32::from(self.body_scroll.max_offset().y),
        )
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

/// A message of a chat: who, what kind (the user's prompt, the agent's answer, a failure, an answer as it comes) and
/// the whole text as Markdown (agents answer in it), selectable.
fn message(member: &str, kind: &str, text: &str, actor_changed: bool, t: &Tokens) -> AnyElement {
    let color = match kind {
        "thread.fail" => t.critical,
        "answering" => t.accent,
        _ => t.text,
    };
    div()
        .id(SharedString::from(format!(
            "message-{kind}-{member}-{}",
            text.len()
        )))
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
                            .child(if kind == "answering" {
                                format!("{member} · answering")
                            } else {
                                member.to_string()
                            }),
                    )
                })
                .child(
                    div().text_size(T_SM).text_color(color).child(
                        TextView::markdown(
                            SharedString::from(format!(
                                "message-text-{kind}-{member}-{}",
                                text.len()
                            )),
                            text.to_string(),
                        )
                        .selectable(true),
                    ),
                ),
        )
        .test_support()
        .into_any_element()
}

impl LegionApp {
    /// A thread tab's head: its title, state, dossier and account, and its members, each with its account and that
    /// account's use; an operator member's account opens the picker.
    fn thread_header(&self, v: &ThreadView, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let members: Vec<Div> = v
            .members
            .iter()
            .filter(|m| m.state != "left")
            .map(|m| {
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
                    .child(self.account_chip(&m.name, m.account.as_deref(), Some(&m.run), t, cx))
            })
            .collect();
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
                "chat {} · dossier {}",
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
        let picking = self
            .screen
            .picking
            .as_ref()
            .filter(|p| !p.is_empty() && v.members.iter().any(|m| m.name == **p));
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
            .children(picking.map(|p| self.account_picker(v.thread.id, Some(p), t, cx)))
    }

    /// The account of a member (`run` given) or of the chat bar (`run` none, `who` ""), with its use. Only an operator
    /// member and the chat bar pick their account; another member's chip says why it cannot.
    fn account_chip(
        &self,
        who: &str,
        account: Option<&str>,
        run: Option<&str>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let usage = self.screen.usage_of(account);
        let state = self.screen.account_state(account);
        let name = account.unwrap_or("current login");
        let text = if usage.is_empty() {
            name.to_string()
        } else {
            format!("{name} · {usage}")
        };
        let why: Option<&'static str> = match run {
            None | Some("operator") => None,
            Some("teammate") => Some("Uses the account of its lead"),
            Some("subagent") => Some("Uses the account of its main session"),
            Some(_) => Some("Resume its session on another account to change this"),
        };
        let chip = div()
            .id(SharedString::from(format!("account-{who}")))
            .flex_none()
            .h_flex()
            .gap(S1)
            .px(S2)
            .py(px(1.))
            .rounded_full()
            .bg(t.accent.opacity(0.10))
            .text_size(T_XS)
            .text_color(if why.is_some() { t.muted } else { t.accent })
            .child(icon(IconName::User, px(11.), t.muted))
            .child(text);
        if let Some(why) = why {
            return chip
                .tooltip(move |window, cx| Tooltip::new(why).build(window, cx))
                .test_support()
                .into_any_element();
        }
        // the step that gets the use read is a button beside the chip, not a sentence
        let fix = self.screen.usage_fix(account).map(|a| {
            let button = legionctl_api::ViewButton {
                label: a.name.clone(),
                op: a.op,
                input: a.input,
                confirm: false,
            };
            Button::new(SharedString::from(format!("account-fix-{who}")))
                .xsmall()
                .label(a.name)
                .on_click(cx.listener(move |app, _, _, cx| app.act(button.clone(), cx)))
        });
        let who = who.to_string();
        let chip = crate::theme::control(chip, "Choose an account", t)
            .cursor_pointer()
            .tooltip(move |window, cx| {
                Tooltip::new(format!(
                    "{state}
Choose an account"
                ))
                .build(window, cx)
            })
            .on_click(cx.listener(move |app, _, _, cx| {
                app.screen.picking = match &app.screen.picking {
                    Some(p) if *p == who => None,
                    _ => Some(who.clone()),
                };
                cx.stop_propagation();
                cx.notify();
            }))
            .test_support();
        div()
            .h_flex()
            .gap(S1)
            .child(chip)
            .children(fix)
            .into_any_element()
    }

    /// The accounts with their use, for `member` of `thread` (none: the chat bar): the current login and clauth's
    /// other profiles; a pick runs `thread.account` with its approval.
    fn account_picker(
        &self,
        thread: i64,
        member: Option<&str>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self
            .screen
            .accounts
            .iter()
            .any(|a| a.source == "current")
            .then_some((None, "current".to_string(), "current login".to_string()));
        let entries = current.into_iter().chain(
            self.screen
                .accounts
                .iter()
                .filter(|a| a.source != "current")
                .map(|a| {
                    let label = if a.active {
                        format!("{} ({}, active)", a.name, a.source)
                    } else {
                        format!("{} ({})", a.name, a.source)
                    };
                    (Some(a.clone()), a.name.clone(), label)
                }),
        );
        let picks = entries.map(|(account, name, text)| {
            let usage = self
                .screen
                .usage_of((account.is_some()).then_some(name.as_str()));
            let chip = div()
                .id(SharedString::from(format!("pick-{name}")))
                .flex_none()
                .px(S2)
                .py(px(2.))
                .rounded(R_SM)
                .border_1()
                .border_color(t.border)
                .bg(t.raised)
                .text_size(T_XS)
                .child(if usage.is_empty() {
                    text
                } else {
                    format!("{text} · {usage}")
                });
            let pick = account.map(|a| a.name);
            let input = json!({"thread": thread, "member": member, "account": pick});
            let label = match (member, &pick) {
                (Some(m), Some(a)) => format!("Run {m} on account {a}"),
                (Some(m), None) => format!("Run {m} on the current login"),
                (None, Some(a)) => format!("Chat on account {a}"),
                (None, None) => "Chat on the current login".into(),
            };
            crate::theme::control(chip, label.clone(), t)
                .text_color(t.text)
                .cursor_pointer()
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.screen.picking = None;
                    app.act(
                        legionctl_api::ViewButton {
                            label: label.clone(),
                            op: "thread.account".into(),
                            input: input.clone(),
                            confirm: false,
                        },
                        cx,
                    );
                }))
                .test_support()
        });
        div()
            .id("account-picker")
            .h_flex()
            .flex_wrap()
            .gap(S1)
            .when(self.screen.accounts.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(T_XS)
                        .text_color(t.muted)
                        .child("No login: log in to an account on the Accounts page"),
                )
            })
            .children(picks)
            .test_support()
            .into_any_element()
    }
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
        let when = legionctl_core::views::local(r.at);
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
                                    .child(crate::widgets::text("detail", chat::log_detail(r))),
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
                            crate::theme::control(
                                div().id(SharedString::from(format!("more-{id}"))),
                                if open { "Less" } else { "More" },
                                t,
                            )
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
                            d.child(div().text_size(T_XS).text_color(t.text2).children(
                                more.lines().enumerate().map(|(i, l)| {
                                    div().child(crate::widgets::text(
                                        format!("line-{i}"),
                                        l.to_string(),
                                    ))
                                }),
                            ))
                        })
                    }),
            )
            .test_support()
            .into_any_element()
    }

    /// Where the user's thread works: its scratch folder, a repository at its own branch, or a plain folder, and
    /// the environment of its runs; a button attaches a repository or changes it (the form of `thread.repo`).
    fn thread_place(th: &ThreadInfo, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let text = place_line(th);
        let id = th.id;
        let label = "Change where the chat works";
        div()
            .h_flex()
            .flex_wrap()
            .gap(S2)
            .text_size(T_XS)
            .text_color(t.muted)
            .child(text)
            .child("· sandbox, model gateway only")
            .child(
                Button::new("chat-place-change")
                    .small()
                    .outline()
                    .label(label)
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.choose_place(crate::Then::ChatPlace, Some(id), cx);
                    })),
            )
    }

    /// Make `path` the app's project: every view, chat and button uses it, and the app keeps it after a restart.
    pub(crate) fn set_project(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.use_project(path);
        self.remember("project", &json!(path), cx);
    }

    /// `path` is the folder of every call from now on (the client sends it with each), and new chats work in it.
    pub(crate) fn use_project(&mut self, path: &Path) {
        path.clone_into(&mut self.screen.project);
        self.client.set_folder(path);
        self.screen.chat_place.get_or_insert_default();
    }

    /// The environment step (DESIGN-15): the form of a folder (for a chat, the app's project first) and, for a chat,
    /// the base ref. The folder becomes the app's project (DESIGN-20 rule 7: set once, every tab, chat and view uses it);
    /// `then` says what else Continue does: a new chat works in it, an existing chat (`thread`) moves to it with
    /// `thread.repo` and its approval, a view tab reads its code.
    pub(crate) fn choose_place(
        &mut self,
        then: crate::Then,
        thread: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        let folder = legionctl_api::form::Field {
            name: "folder".into(),
            kind: legionctl_api::form::Kind::Text,
            required: true,
            help: match then {
                crate::Then::TabProject => {
                    "A folder in a git repository: the view shows its code".into()
                }
                _ => "A git repository gets its own branch; a plain folder gets a copy".into(),
            },
            default: String::new(),
        };
        let base = legionctl_api::form::Field {
            name: "base".into(),
            kind: legionctl_api::form::Kind::Text,
            required: false,
            help: "The ref that the chat's branch starts from; empty: the current branch".into(),
            default: String::new(),
        };
        let fields = match then {
            crate::Then::TabProject => vec![folder],
            _ => vec![folder, base],
        };
        // a view tab asks because its folder is no repository: its form starts empty
        let mut input = match then {
            crate::Then::TabProject => json!({}),
            _ => json!({"folder": self.screen.project}),
        };
        if let Some(id) = thread {
            input["thread"] = json!(id);
        }
        self.state.pending = None;
        // the form shows at the top of the tab
        self.body_scroll
            .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        let values = legionctl_api::form::texts(&input);
        let button = legionctl_api::ViewButton {
            label: match then {
                crate::Then::TabProject => "Choose a repository",
                _ => "Where the chat works",
            }
            .into(),
            op: "thread.repo".into(),
            input,
            confirm: false,
        };
        self.state.form = Some(crate::Form::new(button, then, fields, values, true));
        cx.notify();
    }

    /// Continue of the environment step: the folder must exist; it becomes the app's project, then the new chat
    /// works in it, the chat moves to it, or the view tab reads its code.
    pub(crate) fn submit_place(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.state.form.as_mut() else {
            return;
        };
        let text = |k: &str| form.values.get(k).map_or("", |v| v.trim()).to_string();
        let (folder, base) = (text("folder"), text("base"));
        if folder.is_empty() {
            form.error = Some("Give a folder".into());
            cx.notify();
            return;
        }
        let path = self.screen.project.join(&folder);
        if !path.is_dir() {
            form.error = Some(format!("{} is not a folder", path.display()));
            cx.notify();
            return;
        }
        let (then, thread) = (form.then, form.button.input["thread"].as_i64());
        self.state.form = None;
        self.set_project(&path, cx);
        match (then, thread) {
            // the tab keeps its key: read its view again for the new project
            (crate::Then::TabProject, _) => self.render_view(cx),
            (_, Some(id)) => {
                let mut input = json!({"thread": id});
                if !base.is_empty() {
                    input["base"] = json!(base);
                }
                self.confirm(
                    legionctl_api::ViewButton {
                        label: "Change where the chat works".into(),
                        op: "thread.repo".into(),
                        input,
                        confirm: false,
                    },
                    cx,
                );
            }
            (_, None) => self.screen.chat_place = Some(base),
        }
        cx.notify();
    }

    /// A thread tab: its title and facts, its members, its rules and its timeline.
    fn thread_tab(&self, id: i64, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let Some(v) = self.screen.shown.get(&id) else {
            return div()
                .text_color(t.muted)
                .child(match &self.screen.thread_error {
                    Some(e) => format!("Cannot read chat {id}: {e}"),
                    None => "Reading the chat…".into(),
                });
        };
        let now = unix_now();
        let mut last = None;
        let mut rows: Vec<AnyElement> = v
            .log
            .iter()
            .map(|r| {
                let changed = last.as_deref() != Some(r.member.as_str());
                last = Some(r.member.clone());
                if matches!(
                    r.kind.as_str(),
                    "thread.say" | "thread.answer" | "thread.fail"
                ) {
                    return message(&r.member, &r.kind, &chat::log_detail(r), changed, t);
                }
                self.log_row(r, changed, now, t, cx)
            })
            .collect();
        // an adapter install that failed: the same message again
        if !self.screen.live.contains_key(&id)
            && let Some(fail) = v
                .log
                .last()
                .filter(|r| r.kind == "thread.fail" && r.payload["retry"] == true)
            && let Some(prompt) = v
                .log
                .iter()
                .rev()
                .find(|r| r.kind == "thread.say")
                .and_then(|r| r.payload["text"].as_str())
        {
            let (agent, prompt) = (fail.member.clone(), prompt.to_string());
            rows.push(
                div()
                    .h_flex()
                    .pl(px(32.))
                    .child(
                        Button::new("retry")
                            .small()
                            .primary()
                            .label("Retry")
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.say(id, &agent, &prompt, prompt.clone(), cx);
                            })),
                    )
                    .into_any_element(),
            );
        }
        // the answer as the agent writes it
        if let Some((_, so_far)) = self.screen.live.get(&id) {
            let agent = self.screen.agent().unwrap_or_else(|| "agent".into());
            let text = if so_far.is_empty() {
                "…"
            } else {
                so_far.as_str()
            };
            rows.push(message(&agent, "answering", text, true, t));
        }
        div()
            .v_flex()
            .gap(S4)
            .child(self.thread_header(v, t, cx))
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
                        d.child(div().text_color(t.muted).child("No messages yet"))
                    })
                    .test_support(),
            )
            // where the user's chat works, below its newest message: the tab scrolls there
            .when(v.thread.owner.is_none(), |d| {
                d.child(Self::thread_place(&v.thread, t, cx))
            })
    }

    /// The chat bar of the tab: the agent chip and the text field.
    fn chat_bar(
        &self,
        width: gpui_kit::DefiniteLength,
        t: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let focused = self.chat_focus.is_focused(window);
        let agent = self.screen.agent();
        let chip = crate::theme::control(div().id("agent-chip"), "Choose the agent", t)
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
            .child(agent.clone().unwrap_or_else(|| "Choose an agent".into()))
            .tooltip(|window, cx| Tooltip::new("Choose the agent").build(window, cx))
            .on_click(cx.listener(|app, _, window, cx| {
                *app.screen.chat_mut() = "/agent ".into();
                app.chat_focus.focus(window, cx);
                cx.stop_propagation();
                cx.notify();
            }))
            .test_support();
        div()
            .id("chat")
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
            .child(self.chat_input(cx))
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
                .track_scroll(&self.list_scroll)
                .v_flex()
                .p(S1)
                .rounded(R_MD)
                .border_1()
                .border_color(t.border)
                .bg(t.raised)
                .text_size(T_XS)
                .children(entries.iter().take(SHOWN).enumerate().map(|(i, e)| {
                    div()
                        .id(SharedString::from(format!("completion-{i}")))
                        .h_flex()
                        .gap(S3)
                        .px(S2)
                        .py(S1)
                        .rounded(R_SM)
                        .when(self.screen.list.pick == Some(i), |d| {
                            d.bg(t.accent.opacity(0.18))
                        })
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
                    .track_focus(&self.body_focus)
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
                    .children(
                        self.screen
                            .thread
                            .as_ref()
                            .filter(|_| self.screen.picking.as_deref() == Some(""))
                            .map(|v| {
                                div()
                                    .w(width)
                                    .child(self.account_picker(v.thread.id, None, t, cx))
                            }),
                    )
                    // the chat bar's account, above the bar: the bar keeps its width for the text
                    .when(self.screen.thread.is_some(), |d| {
                        d.child(div().w(width).h_flex().child(self.account_chip(
                            "",
                            self.screen.chat_account().as_deref(),
                            None,
                            t,
                            cx,
                        )))
                    })
                    .child(self.chat_bar(width, t, window, cx))
                    .test_support(),
            )
            .test_support()
            .into_any_element()
    }

    /// A new chat: its dossier and its environment step (where it works, the sandbox), changeable before the first
    /// message.
    fn chat_place_card(&self, dossier: &str, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let folder = self.screen.project.display();
        let place = match &self.screen.chat_place {
            Some(base) if base.is_empty() => format!("Works in {folder}"),
            Some(base) => format!("Works in {folder} from {base}"),
            None => "Works in a new folder of this chat".into(),
        };
        div()
            .v_flex()
            .gap(S2)
            .text_color(t.muted)
            .child(format!(
                "A new chat in dossier {dossier}: write your message below. The first message starts the chat; it \
                 keeps the conversation."
            ))
            .child(
                div()
                    .id("chat-place")
                    .h_flex()
                    .flex_wrap()
                    .gap(S2)
                    .text_size(T_XS)
                    .child(crate::widgets::text("chat-place-text", place))
                    .child("· sandbox, model gateway only")
                    .child(
                        Button::new("chat-place-change")
                            .small()
                            .outline()
                            .label("Change where the chat works")
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.choose_place(crate::Then::ChatPlace, None, cx);
                            })),
                    )
                    .test_support(),
            )
    }

    /// The content of the tab that shows.
    pub(crate) fn tab_content(&self, t: &Tokens, width: f32, cx: &mut Context<Self>) -> Div {
        match self.tab().cloned() {
            None => div()
                .text_color(t.muted)
                .child("Choose an item on the left, or type a message"),
            Some(Tab::Thread { id }) => self.thread_tab(id, t, cx),
            Some(Tab::Chat) => match &self.screen.dossier {
                Some(d) => self.chat_place_card(d, t, cx),
                None => div()
                    .v_flex()
                    .gap(S2)
                    .child(div().text_color(t.muted).child(
                        "Choose a dossier for the chat: it keeps the conversation. Click Use on a dossier, or New \
                         dossier.",
                    ))
                    .children(self.screen.view_data.get(PICKER).map(|data| {
                        div()
                            .v_flex()
                            .gap(S2)
                            .child(Self::buttons(&data.name, "action", &data.actions, cx))
                            .child(self.widget(data, width, t, cx, false))
                    })),
            },
            Some(Tab::View { .. }) => match self.screen.view() {
                Some(data) => div().child(self.view_tab(data, width, t, cx)),
                None => div().text_color(t.muted).child("Reading the view…"),
            },
        }
    }
}
