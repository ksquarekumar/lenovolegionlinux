//! The thread screen (design 23, Part C, P7; DESIGN-09 G6, Rule 1). Pane 1 lists the views (`views list`: the seeds,
//! the user's and the repository's) and the output of the last command, pane 2 shows the thread's events and the
//! runs of this screen, pane 3 shows the chosen view in its terminal form (`views render`: the same text as
//! `legionctl views render`) with its buttons below it. A button runs its operation with its own approval, as in the
//! app: a read at once, a change with a dry run shows its plan first, another change asks; Enter applies. A button
//! whose operation needs input puts the operation's CLI words into the chat bar. Panes 1 and 2 fold to one column
//! each. The chat bar is at the bottom centre of pane 3: it grows wider on demand and taller with its text. A line
//! that starts with `/` is a command: an operation of the registry with the CLI's words, a command of the screen
//! (`/fold`, `/view <name> [name=value...]`, `/change <words>`) or a command of the attached agent (`/agent:<name>`,
//! from its ACP `available_commands_update`). Other text is an intent (`ask`): the stored view that answers it, else
//! the composer's view, which then shows. A person and an agent get the same screen and the same commands.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::ArgMatches;
use legionctl_api::chat::{self, Action, Entry};
use legionctl_api::form::{self, Route};
use legionctl_api::{
    AgentCommand, ClientPath, Empty, OPS, Render as _, RunOutput, SessionIdIn, ThreadIdIn,
    ThreadView, ViewAnswer, ViewButton, ViewData, ViewInfo, ViewRenderIn, ViewsListIn, op,
};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};
use serde_json::{Value, json};

use legionctl_api::client::Client;

/// What pane 3 shows: a view, or the output of the last command.
#[derive(Clone, Copy, PartialEq)]
enum Shown {
    View,
    Output,
}

/// A command that runs in the background: the operation and the line that started it (its row in pane 2); a
/// button's result goes under the buttons, not to the output.
struct Running {
    op: &'static str,
    row: usize,
    button: bool,
    task: tokio::task::JoinHandle<Result<Value>>,
}

#[derive(PartialEq)]
enum Focus {
    View,
    Chat,
}

/// A button that waits for Enter: its route and what it will do (the dry run's plan, or its operation and input).
struct Pending {
    button: ViewButton,
    route: Route,
    text: String,
}

struct Screen {
    client: Arc<Client>,
    repo: PathBuf,
    views: Vec<ViewInfo>,
    /// The view of pane 3 and its parameters (`name=value`).
    view: String,
    params: Vec<String>,
    shown: Shown,
    data: Result<Option<ViewData>, String>,
    read_at: Option<Instant>,
    thread_at: Option<Instant>,
    /// Pane 3's first row, and the selected button.
    scroll: u16,
    selected: usize,
    pending: Option<Pending>,
    /// The last button's result, or an answer's words.
    message: String,
    folded: [bool; 2],
    focus: Focus,
    chat: String,
    /// The chat bar's width in percent of pane 3.
    chat_width: u16,
    /// The output view: the text of the last command.
    output: String,
    thread_arg: Option<i64>,
    thread: Result<Option<ThreadView>, String>,
    /// This screen's commands for pane 2, each with its result once it ended.
    runs: Vec<(String, Option<bool>)>,
    running: Option<Running>,
    agent: Option<String>,
    dossier: Option<String>,
    /// The session and the ACP session of the agent's last run: the next prompt goes on in it.
    attached: Option<(String, Option<String>)>,
    commands: Vec<AgentCommand>,
}

/// The buttons of a view and of its parts, in order: the view's own, then each row's with the row's first cell.
fn buttons(data: &ViewData) -> Vec<ViewButton> {
    let mut out = data.actions.clone();
    for (row, actions) in data.rows.iter().zip(&data.row_actions) {
        let first = row.first().map_or("", String::as_str);
        out.extend(actions.iter().map(|b| ViewButton {
            label: format!("{first}: {}", b.label),
            ..b.clone()
        }));
    }
    for part in &data.parts {
        out.extend(buttons(part));
    }
    out
}

/// Text cut into rows of `width` characters: the chat bar's rows, so its height is exact.
fn rows_of(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    text.split('\n')
        .flat_map(|line| {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                return vec![String::new()];
            }
            chars
                .chunks(width)
                .map(|c| c.iter().collect())
                .collect::<Vec<String>>()
        })
        .collect()
}

impl Screen {
    /// The view's data of its last read.
    fn read(&self) -> Option<&ViewData> {
        self.data.as_ref().ok().and_then(Option::as_ref)
    }

    /// The `/` commands that match the chat bar's text.
    fn completions(&self) -> Vec<Entry> {
        if self.focus != Focus::Chat {
            return Vec::new();
        }
        let screen = ["1", "2"]
            .iter()
            .map(|n| (format!("/fold {n}"), format!("Fold or open pane {n}")))
            .chain(self.views.iter().map(|v| {
                (
                    format!("/view {}", v.name),
                    format!("Show {} in pane 3", v.title.as_deref().unwrap_or(&v.name)),
                )
            }))
            .chain(std::iter::once((
                "/change ".to_string(),
                "Change the view of pane 3 (the agent makes its next version)".to_string(),
            )))
            .collect();
        chat::completions(&self.chat, chat::entries(&self.commands, screen))
    }

    fn refresh(&mut self, runtime: &tokio::runtime::Runtime) {
        if self
            .thread_at
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            let client = &self.client;
            let thread = runtime.block_on(async {
                let id = match self.thread_arg {
                    Some(id) => Some(id),
                    None => client
                        .call::<op::ThreadList>(&Empty {})
                        .await?
                        .iter()
                        .rev()
                        .find(|t| t.state == "open")
                        .map(|t| t.id),
                };
                match id {
                    Some(thread) => Ok(Some(
                        client
                            .call::<op::ThreadShow>(&ThreadIdIn { thread })
                            .await?,
                    )),
                    None => Ok(None),
                }
            });
            self.thread = thread.map_err(|e: anyhow::Error| format!("{e:#}"));
            self.thread_at = Some(Instant::now());
        }
        // a view reads again when it changed, after a command, and at its refresh.every
        let every = self
            .read()
            .and_then(|d| d.every)
            .map(|s| Duration::from_secs(u64::from(s)));
        if self
            .read_at
            .is_some_and(|t| every.is_none_or(|every| t.elapsed() < every))
        {
            return;
        }
        let repo = ClientPath(self.repo.clone());
        let (views, data) = runtime.block_on(async {
            let views = self
                .client
                .call::<op::ViewsList>(&ViewsListIn { repo: repo.clone() })
                .await;
            let data = self
                .client
                .call::<op::ViewsRender>(&ViewRenderIn {
                    name: self.view.clone(),
                    params: self.params.clone(),
                    repo,
                })
                .await;
            (views, data)
        });
        match views {
            Ok(views) => self.views = views,
            Err(e) => self.message = format!("the views: {e:#}"),
        }
        self.data = data.map(Some).map_err(|e| format!("{e:#}"));
        self.read_at = Some(Instant::now());
    }

    /// Show the view `name` with `params` in pane 3.
    fn open(&mut self, name: &str, params: Vec<String>) {
        if name != self.view {
            self.scroll = 0;
            self.selected = 0;
        }
        self.view = name.to_string();
        self.params = params;
        self.shown = Shown::View;
        self.pending = None;
        self.read_at = None;
    }

    /// The entries of pane 1: the views, then the output.
    fn entries(&self) -> Vec<&str> {
        self.views
            .iter()
            .map(|v| v.name.as_str())
            .chain(std::iter::once("output"))
            .collect()
    }

    /// The previous (`-1`) or next (`1`) entry of pane 1.
    fn step(&mut self, by: isize) {
        let entries = self.entries();
        let at = match self.shown {
            Shown::Output => entries.len() - 1,
            Shown::View => entries.iter().position(|e| *e == self.view).unwrap_or(0),
        };
        let next = at.saturating_add_signed(by).min(entries.len() - 1);
        if next == entries.len() - 1 {
            self.shown = Shown::Output;
        } else {
            let name = entries[next].to_string();
            self.open(&name, Vec::new());
        }
    }

    /// Start `op` with `input` in the background, as the row `line` in pane 2.
    fn start(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        op: &'static str,
        input: Value,
        line: &str,
        button: bool,
    ) {
        if self.running.is_some() {
            self.message = "a command runs: wait for its end".into();
            if !button {
                self.chat = line.to_string();
            }
            return;
        }
        let client = self.client.clone();
        self.runs.push((line.to_string(), None));
        self.message = format!("running: {line}");
        self.running = Some(Running {
            op,
            row: self.runs.len() - 1,
            button,
            task: runtime.spawn(async move { client.call_value(op, &input).await }),
        });
    }

    /// When the running command ended: its text under the buttons (a button's) or in the output view; an answer's
    /// view shows; a run's session is attached; the view reads again.
    fn collect(&mut self, runtime: &tokio::runtime::Runtime) {
        if !self.running.as_ref().is_some_and(|r| r.task.is_finished()) {
            return;
        }
        let Some(running) = self.running.take() else {
            return;
        };
        let result = runtime
            .block_on(running.task)
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
        let (text, ok) = match result {
            Ok(value) => {
                if running.op == "session.run"
                    && let Ok(run) = serde_json::from_value::<RunOutput>(value.clone())
                {
                    self.attach(runtime, &run);
                }
                if running.op == "ask"
                    && let Ok(answer) = serde_json::from_value::<ViewAnswer>(value.clone())
                {
                    self.answered(runtime, &answer);
                }
                legionctl_api::render(running.op, value)
                    .unwrap_or_else(|e| (format!("{e:#}"), false))
            }
            Err(e) => (format!("{e:#}"), false),
        };
        self.runs[running.row].1 = Some(ok);
        self.read_at = None;
        if running.button {
            self.message = if text.trim().is_empty() {
                "done".into()
            } else {
                text
            };
        } else if running.op != "ask" || self.shown == Shown::Output {
            self.output = text;
            self.shown = Shown::Output;
        }
    }

    /// An intent's answer: its view shows in pane 3 with the composer's words under it, or why there is none.
    fn answered(&mut self, runtime: &tokio::runtime::Runtime, answer: &ViewAnswer) {
        if let Some(run) = &answer.run {
            self.attach(runtime, run);
        }
        match &answer.view {
            Some(name) => {
                let params = if *name == self.view {
                    self.params.clone()
                } else {
                    Vec::new()
                };
                self.open(name, params);
            }
            None => self.shown = Shown::Output,
        }
        self.message = answer.error.as_ref().map_or_else(
            || answer.text.clone(),
            |e| format!("the view failed its check: {e}"),
        );
    }

    fn attach(&mut self, runtime: &tokio::runtime::Runtime, run: &RunOutput) {
        self.attached = Some((run.id.clone(), run.acp.clone()));
        let input = SessionIdIn { id: run.id.clone() };
        match runtime.block_on(self.client.call::<op::SessionCommands>(&input)) {
            Ok(commands) => self.commands = commands,
            Err(e) => self.message = format!("the agent's commands: {e:#}"),
        }
    }

    /// The chat bar's line: a command, an intent, or a command of the agent.
    fn submit(&mut self, runtime: &tokio::runtime::Runtime) {
        let line = std::mem::take(&mut self.chat).trim().to_string();
        if line.is_empty() {
            return;
        }
        match chat::action(&line, &self.commands, &["fold", "view", "change"]) {
            Action::Prompt(prompt) if prompt.starts_with('/') => {
                self.prompt(runtime, &prompt, &line);
            }
            Action::Prompt(intent) => self.ask(runtime, &intent, None, &line),
            Action::Operation(op, input) => self.start(runtime, op.name, input, &line, false),
            Action::Screen(words) => match words.iter().map(String::as_str).collect::<Vec<_>>()[..]
            {
                ["fold", n @ ("1" | "2")] => {
                    let i = usize::from(n == "2");
                    self.folded[i] = !self.folded[i];
                }
                ["view", name, ..] => self.open(name, words[2..].to_vec()),
                ["change", ..] if words.len() > 1 => {
                    let view = self.view.clone();
                    self.ask(runtime, &words[1..].join(" "), Some(&view), &line);
                }
                _ => {
                    self.message =
                        "/fold 1, /fold 2, /view <name> [name=value...] or /change <words>".into();
                }
            },
            Action::Refused(why) => self.message = why,
        }
    }

    /// `intent` (`ask`): the stored view that answers it, else the composer's view; with `view`, its next version.
    fn ask(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        intent: &str,
        view: Option<&str>,
        line: &str,
    ) {
        let params = if view.is_some() {
            self.params.clone()
        } else {
            Vec::new()
        };
        let input = json!({
            "intent": intent, "view": view, "params": params, "agent": self.agent, "dossier": self.dossier,
            "repo": self.repo, "acp": self.attached.as_ref().and_then(|(_, acp)| acp.clone()),
        });
        self.start(runtime, "ask", input, line, false);
    }

    /// An agent's command (`/agent:<name> ...`) as a headless run that goes on in the agent's last ACP session.
    fn prompt(&mut self, runtime: &tokio::runtime::Runtime, prompt: &str, line: &str) {
        let Some(agent) = self.agent.clone() else {
            self.message =
                "no agent: start the screen with --agent <name> (config session.acp)".into();
            return;
        };
        let input = json!({
            "agent": agent,
            "prompt": prompt,
            "dossier": self.dossier,
            "repo": self.repo,
            "acp": self.attached.as_ref().and_then(|(_, acp)| acp.clone()),
        });
        self.start(runtime, "session.run", input, line, false);
    }

    /// The selected button, with its operation's approval: a read runs; a change with a dry run shows its plan; another
    /// change asks. An operation that needs input gets its CLI words in the chat bar.
    fn take(&mut self, runtime: &tokio::runtime::Runtime) {
        let Some(button) = self
            .read()
            .and_then(|d| buttons(d).into_iter().nth(self.selected))
        else {
            return;
        };
        let Some(info) = OPS.iter().find(|o| o.name == button.op && !o.internal()) else {
            self.message = format!(
                "the button {} names no operation {}",
                button.label, button.op
            );
            return;
        };
        if form::needed(info, &button.input) {
            let required: Vec<String> = form::fields(info)
                .into_iter()
                .filter(|f| f.required)
                .map(|f| f.name)
                .collect();
            self.message = format!(
                "{} needs {}: type them in the chat bar",
                info.name,
                required.join(", ")
            );
            self.chat = format!("/{} ", info.name.replace('.', " "));
            self.focus = Focus::Chat;
            return;
        }
        let route = form::route(info);
        let text = match route {
            Route::Run => {
                self.start(
                    runtime,
                    info.name,
                    button.input.clone(),
                    &button.label,
                    true,
                );
                return;
            }
            Route::Plan => {
                let mut input = button.input.clone();
                input["dry_run"] = Value::Bool(true);
                runtime
                    .block_on(self.client.call_value(info.name, &input))
                    .and_then(|v| legionctl_api::render(info.name, v))
                    .map_or_else(|e| format!("{e:#}"), |(text, _)| text)
            }
            Route::Confirm => format!("runs {} {}", info.name, button.input),
        };
        self.pending = Some(Pending {
            button,
            route,
            text,
        });
    }

    /// Enter on a pending button: its operation runs (a dry-run operation for real).
    fn apply(&mut self, runtime: &tokio::runtime::Runtime) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let Some(info) = OPS.iter().find(|o| o.name == pending.button.op) else {
            return;
        };
        let mut input = pending.button.input;
        if pending.route == Route::Plan {
            input["dry_run"] = Value::Bool(false);
        }
        self.start(runtime, info.name, input, &pending.button.label, true);
    }

    /// A key in the chat bar.
    fn chat_key(&mut self, runtime: &tokio::runtime::Runtime, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Enter if alt => self.chat.push('\n'),
            KeyCode::Char('j') if control => self.chat.push('\n'),
            KeyCode::Enter => self.submit(runtime),
            KeyCode::Right if control => self.chat_width = (self.chat_width + 10).min(100),
            KeyCode::Left if control => {
                self.chat_width = self.chat_width.saturating_sub(10).max(30);
            }
            KeyCode::Backspace => {
                self.chat.pop();
            }
            KeyCode::Tab => {
                if let Some(first) = self.completions().first()
                    && first.text.starts_with(&self.chat)
                {
                    self.chat = format!("{} ", first.text);
                }
            }
            KeyCode::Esc => self.focus = Focus::View,
            KeyCode::Char(c) if !control && !alt => self.chat.push(c),
            _ => {}
        }
    }

    /// A key on the view; `true` when the screen must quit.
    fn view_key(&mut self, runtime: &tokio::runtime::Runtime, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('/') => {
                self.focus = Focus::Chat;
                self.chat = "/".into();
            }
            KeyCode::Tab => self.focus = Focus::Chat,
            KeyCode::Left => self.step(-1),
            KeyCode::Right => self.step(1),
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.pending = None;
            }
            KeyCode::Down => {
                self.selected += 1;
                self.pending = None;
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::Esc => self.pending = None,
            KeyCode::Enter if self.pending.is_some() => self.apply(runtime),
            KeyCode::Enter => self.take(runtime),
            _ => {}
        }
        let count = self.read().map_or(0, |d| buttons(d).len());
        self.selected = self.selected.min(count.saturating_sub(1));
        false
    }

    /// A key; `true` when the screen must quit.
    fn key(&mut self, runtime: &tokio::runtime::Runtime, key: KeyEvent) -> bool {
        let fold = key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('1') if fold => self.folded[0] = !self.folded[0],
            KeyCode::Char('2') if fold => self.folded[1] = !self.folded[1],
            _ if self.focus == Focus::Chat => self.chat_key(runtime, key),
            _ => return self.view_key(runtime, key),
        }
        false
    }
}

/// A folded pane: one column with its number and name, top to bottom.
fn folded(f: &mut Frame, area: Rect, label: &str) {
    let lines: Vec<Line> = label.chars().map(|c| Line::from(c.to_string())).collect();
    f.render_widget(Paragraph::new(lines).reversed(), area);
}

fn draw_views(f: &mut Frame, area: Rect, s: &Screen) {
    let lines: Vec<Line> = s
        .entries()
        .into_iter()
        .map(|name| {
            let on = match s.shown {
                Shown::Output => name == "output",
                Shown::View => name == s.view,
            };
            let line = Line::from(format!(" {name}"));
            if on { line.reversed() } else { line }
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("1 views")),
        area,
    );
}

fn draw_events(f: &mut Frame, area: Rect, s: &Screen) {
    let (title, mut lines) = match &s.thread {
        Ok(Some(t)) => (
            format!(" 2 thread {}: {} ", t.thread.id, t.thread.title),
            t.log.iter().map(chat::log_line).collect(),
        ),
        Ok(None) => (" 2 events ".to_string(), vec!["no open thread".to_string()]),
        Err(e) => (" 2 events ".to_string(), vec![format!("the thread: {e}")]),
    };
    if !s.runs.is_empty() {
        lines.push("runs".into());
        lines.extend(s.runs.iter().map(|(line, ok)| {
            let mark = match ok {
                None => "…",
                Some(true) => "✓",
                Some(false) => "✗",
            };
            format!("{mark} {}", line.replace('\n', " ↵ "))
        }));
    }
    // the newest rows that fit
    let room = usize::from(area.height.saturating_sub(2));
    let shown: Vec<Line> = lines[lines.len().saturating_sub(room)..]
        .iter()
        .map(|l| Line::from(l.as_str()))
        .collect();
    f.render_widget(
        Paragraph::new(shown).block(Block::bordered().title(title)),
        area,
    );
}

/// Pane 3: the view's terminal form, and below it its buttons with the selected one, a pending button's plan and the
/// last result.
fn draw_view(f: &mut Frame, area: Rect, s: &Screen) {
    if s.shown == Shown::Output {
        f.render_widget(
            Paragraph::new(s.output.as_str()).block(Block::bordered().title(" output ")),
            area,
        );
        return;
    }
    let (title, text, list) = match &s.data {
        Err(e) => (
            format!(" {} ", s.view),
            format!("cannot read the view: {e}"),
            Vec::new(),
        ),
        Ok(None) => (format!(" {} ", s.view), "reading…".to_string(), Vec::new()),
        Ok(Some(d)) => (
            format!(" {} · version {} ", d.title, d.version),
            d.text(),
            buttons(d),
        ),
    };
    let mut below: Vec<Line> = list
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let line = Line::from(format!(
                "{} {}  ({})",
                if i == s.selected { "▶" } else { " " },
                b.label,
                b.op
            ));
            if i == s.selected { line.bold() } else { line }
        })
        .collect();
    // the selected button stays in sight: at most 8 rows of buttons
    let first = s.selected.saturating_sub(7);
    below = below.split_off(first.min(below.len()));
    below.truncate(8);
    if let Some(p) = &s.pending {
        below.extend(p.text.lines().map(|l| Line::from(l.to_string()).cyan()));
        below.push(Line::from("Enter applies · Esc cancels").cyan());
    } else if !s.message.is_empty() {
        below.extend(s.message.lines().map(|l| Line::from(l.to_string())));
    }
    let height = if below.is_empty() {
        0
    } else {
        u16::try_from(below.len() + 2)
            .unwrap_or(u16::MAX)
            .min(area.height / 2)
    };
    let [top, bottom] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(height)]).areas(area);
    f.render_widget(
        Paragraph::new(text)
            .scroll((s.scroll, 0))
            .block(Block::bordered().title(title)),
        top,
    );
    if height > 0 {
        f.render_widget(
            Paragraph::new(below).block(Block::bordered().title(" buttons ")),
            bottom,
        );
    }
}

/// The chat bar's place at the bottom centre of `pane` (its width, and its height from its text up to 40% of the
/// pane), and its rows.
fn chat_bar(pane: Rect, s: &Screen) -> (Rect, Vec<String>) {
    let width =
        u16::try_from(u32::from(pane.width) * u32::from(s.chat_width) / 100).unwrap_or(pane.width);
    let mut text = s.chat.clone();
    if s.focus == Focus::Chat {
        text.push('▌');
    }
    let rows = rows_of(&text, width.saturating_sub(2));
    let most = (pane.height * 40 / 100).max(3);
    let height = u16::try_from(rows.len() + 2).unwrap_or(u16::MAX).min(most);
    let x = pane.x + (pane.width - width) / 2;
    (
        Rect::new(x, pane.y + pane.height - height, width, height),
        rows,
    )
}

/// The chat bar and the completion list above it.
fn draw_chat(f: &mut Frame, pane: Rect, bar: Rect, rows: &[String], s: &Screen) {
    // the newest rows when the text is taller than the bar
    let skip = rows.len().saturating_sub(usize::from(bar.height - 2));
    let shown: Vec<Line> = rows[skip..]
        .iter()
        .map(|r| Line::from(r.as_str()))
        .collect();
    let title = match (&s.agent, &s.attached) {
        (Some(agent), Some((session, _))) => format!(" chat → {agent} ({session}) "),
        (Some(agent), None) => format!(" chat → {agent} "),
        (None, _) => " chat: commands only (no --agent) ".to_string(),
    };
    let border = if s.focus == Focus::Chat {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    f.render_widget(
        Paragraph::new(shown).block(
            Block::bordered()
                .title(title)
                .border_style(Style::default().fg(border)),
        ),
        bar,
    );
    let entries = s.completions();
    if !entries.is_empty() {
        let room = bar.y.saturating_sub(pane.y);
        let height = u16::try_from(entries.len() + 2)
            .unwrap_or(u16::MAX)
            .min(12)
            .min(room);
        let list = Rect::new(bar.x, bar.y - height, bar.width, height);
        let lines: Vec<Line> = entries
            .iter()
            .map(|e| {
                let line = Line::from(format!("{}  {}", e.text, e.about));
                if e.agent { line.cyan() } else { line }
            })
            .collect();
        f.render_widget(Clear, list);
        f.render_widget(
            Paragraph::new(lines).block(Block::bordered().title(" commands ")),
            list,
        );
    }
}

fn draw(f: &mut Frame, s: &Screen) {
    let [main, help] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
    let [p1, p2, p3] = Layout::horizontal([
        if s.folded[0] {
            Constraint::Length(1)
        } else {
            Constraint::Percentage(7)
        },
        if s.folded[1] {
            Constraint::Length(1)
        } else {
            Constraint::Percentage(30)
        },
        Constraint::Fill(1),
    ])
    .areas(main);
    if s.folded[0] {
        folded(f, p1, "1views");
    } else {
        draw_views(f, p1, s);
    }
    if s.folded[1] {
        folded(f, p2, "2events");
    } else {
        draw_events(f, p2, s);
    }
    let (bar, rows) = chat_bar(p3, s);
    // the view takes pane 3 above the chat bar's rows
    draw_view(f, Rect::new(p3.x, p3.y, p3.width, bar.y - p3.y), s);
    draw_chat(f, p3, bar, &rows, s);
    let keys = if s.focus == Focus::Chat {
        " Enter send · Alt+Enter new line · Tab complete · Ctrl+Left/Right width · Esc views · Alt+1/Alt+2 fold"
    } else {
        " q quit · Left/Right views · Up/Down buttons · Enter a button, then applies · Esc cancel · PgUp/PgDn scroll · / command · Tab chat · Alt+1/Alt+2 fold"
    };
    f.render_widget(Paragraph::new(keys).dim(), help);
}

/// `legionctl tui [--thread <id>] [--agent <name>] [--dossier <name>] [--view <name>]`.
pub fn run(args: &ArgMatches) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let mut screen = Screen {
        client: Arc::new(Client::new("tui")?),
        repo: std::env::current_dir()?,
        views: Vec::new(),
        view: args
            .get_one::<String>("view")
            .cloned()
            .unwrap_or_else(|| "machine-now".into()),
        params: Vec::new(),
        shown: Shown::View,
        data: Ok(None),
        read_at: None,
        thread_at: None,
        scroll: 0,
        selected: 0,
        pending: None,
        message: String::new(),
        folded: [false; 2],
        focus: Focus::View,
        chat: String::new(),
        chat_width: 60,
        output: String::new(),
        thread_arg: args.get_one::<i64>("thread").copied(),
        thread: Ok(None),
        runs: Vec::new(),
        running: None,
        agent: args.get_one::<String>("agent").cloned(),
        dossier: args.get_one::<String>("dossier").cloned(),
        attached: None,
        commands: Vec::new(),
    };
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        loop {
            screen.collect(&runtime);
            screen.refresh(&runtime);
            terminal.draw(|f| draw(f, &screen))?;
            let wait = if screen.running.is_some() { 100 } else { 1000 };
            if event::poll(Duration::from_millis(wait))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && screen.key(&runtime, key)
            {
                return Ok(());
            }
        }
    })();
    ratatui::restore();
    result
}
