//! The thread screen (design 23, Part C, P7). Pane 1 lists the views as icons, pane 2 shows the thread's events and
//! the runs of this screen, pane 3 shows the plugged view: the dashboard (load, machine, workspaces, agent settings),
//! the resources (note 22, each action with its dry run first), the sensors (note 16) or the output of the last
//! command. Panes 1 and 2 fold to one column each. The chat bar is at the bottom centre of pane 3: it grows wider on
//! demand and taller with its text. A line that starts with `/` is a command: an operation of the registry with the
//! CLI's words, a command of the screen (`/fold`, `/view`) or a command of the attached agent (`/agent:<name>`, from
//! its ACP `available_commands_update`). Other text goes to the agent as a prompt (`session run`, in the same ACP
//! session). A person and an agent get the same screen and the same commands.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::ArgMatches;
use legionctl_api::{
    AgentCommand, Class, ClientPath, ConfigFile, Empty, OPS, ProjectIn, Reading, Render as _,
    ResourceActIn, ResourceList, RunOutput, SensorsIn, SessionIdIn, Snapshot, StatusIn, ThreadIdIn,
    ThreadLogRow, ThreadView, Workspace, op,
};
use legionctl_api::{Level, Pressure};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Gauge, Paragraph, Row, Table, TableState};
use serde_json::{Value, json};

use legionctl_api::client::Client;

fn level_color(level: Level) -> Color {
    match level {
        Level::Idle | Level::Normal => Color::Green,
        Level::Busy => Color::Yellow,
        Level::Overloaded => Color::Red,
    }
}

fn pressure_gauge(title: &str, p: Option<&Pressure>) -> Gauge<'static> {
    let pct = p.map_or(0.0, |p| p.some_avg10);
    let color = if pct >= 60.0 {
        Color::Red
    } else if pct >= 20.0 {
        Color::Yellow
    } else {
        Color::Green
    };
    Gauge::default()
        .block(Block::bordered().title(format!("{title} pressure")))
        .gauge_style(Style::default().fg(color))
        .ratio((pct / 100.0).clamp(0.0, 1.0))
        .label(format!("{pct:.0}%"))
}

/// Open workspaces of every dossier.
fn workspaces_table(workspaces: &[Workspace]) -> Table<'_> {
    let rows = workspaces.iter().map(|w| {
        Row::new(vec![
            w.dossier.clone(),
            w.branch.clone(),
            w.base.clone(),
            w.path.display().to_string(),
        ])
    });
    Table::new(
        rows,
        [
            Constraint::Length(16),
            Constraint::Length(24),
            Constraint::Length(10),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(vec!["dossier", "branch", "base", "path"]).bold())
    .block(Block::bordered().title(" workspaces "))
}

/// Agent settings files that exist; invalid ones in red.
fn configs_table(configs: &[ConfigFile]) -> Table<'_> {
    let cfg_rows = configs.iter().filter(|c| c.exists).map(|c| {
        let state = if c.valid == Some(true) {
            c.sections.join(", ")
        } else {
            format!("INVALID: {}", c.error.clone().unwrap_or_default())
        };
        let row = Row::new(vec![c.agent.clone(), c.scope.clone(), state]);
        if c.valid == Some(true) {
            row
        } else {
            row.style(Style::default().fg(Color::Red))
        }
    });
    Table::new(
        cfg_rows,
        [
            Constraint::Length(7),
            Constraint::Length(14),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(vec!["agent", "scope", "sections"]).bold())
    .block(Block::bordered().title(" agent settings "))
}

fn draw_dashboard(
    f: &mut Frame,
    area: Rect,
    s: &Snapshot,
    workspaces: &[Workspace],
    configs: &[ConfigFile],
) {
    let [header, gauges, body] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(8),
    ])
    .areas(area);

    let mode = s.platform_profile.as_deref().unwrap_or("?");
    let fnq = s
        .fn_q_override
        .as_ref()
        .map_or(String::new(), |o| format!(" (Fn+Q: {o})"));
    let temp = s
        .package_temp_c
        .map_or("?".into(), |t| format!("{t:.0} °C"));
    let status = Line::from(vec![
        format!(" {} ", s.power_source).bold(),
        format!("mode {mode}{fnq}  ").into(),
        format!("load {:?} ", s.load.level)
            .fg(level_color(s.load.level))
            .bold(),
        format!(
            "({:.1} on {} CPUs)  CPU {temp}  limits {}/{} W",
            s.load.load1,
            s.load.cpus,
            s.firmware_limits
                .get("ppt_pl1_spl")
                .map_or("?", String::as_str),
            s.firmware_limits
                .get("ppt_pl2_sppt")
                .map_or("?", String::as_str)
        )
        .into(),
    ]);
    f.render_widget(
        Paragraph::new(status).block(Block::bordered().title(" legionctl ")),
        header,
    );

    let [cpu, mem, io] = Layout::horizontal([Constraint::Ratio(1, 3); 3]).areas(gauges);
    f.render_widget(pressure_gauge("CPU", s.load.cpu.as_ref()), cpu);
    f.render_widget(pressure_gauge("Memory", s.load.memory.as_ref()), mem);
    f.render_widget(pressure_gauge("I/O", s.load.io.as_ref()), io);

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(body);
    let mut machine: Vec<Line> = vec![
        format!("GPU mode {} · desktop on {}", s.gpu_mode, s.desktop_gpu).into(),
        format!("governors {:?}", s.governors).into(),
        format!("EPP {:?}", s.energy_performance_preference).into(),
        "".into(),
        "firmware limits".bold().into(),
    ];
    machine.extend(
        s.firmware_limits
            .iter()
            .map(|(k, v)| Line::from(format!("  {k} = {v}"))),
    );
    machine.push("".into());
    machine.push("displays".bold().into());
    machine.extend(
        s.outputs
            .iter()
            .filter(|o| o.connected)
            .map(|d| Line::from(format!("  {} on {} ({})", d.connector, d.card, d.driver))),
    );
    f.render_widget(
        Paragraph::new(machine).block(Block::bordered().title(" machine ")),
        left,
    );

    let [wt_area, cfg_area] =
        Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(right);
    f.render_widget(workspaces_table(workspaces), wt_area);
    f.render_widget(configs_table(configs), cfg_area);
}

/// The resources view: what is selected, and the action whose dry run is shown.
#[derive(Default)]
struct ResourcesState {
    selected: usize,
    /// (id, action, the dry run's text)
    pending: Option<(String, String, String)>,
    /// The last result or error.
    message: String,
}

fn draw_resources(f: &mut Frame, area: Rect, list: &ResourceList, view: &ResourcesState) {
    let [table_area, detail] =
        Layout::vertical([Constraint::Min(8), Constraint::Length(8)]).areas(area);
    let rows = list.resources.iter().map(|r| {
        let actions: Vec<String> = r
            .actions
            .iter()
            .enumerate()
            .map(|(i, a)| format!("{} {}", i + 1, a.name))
            .collect();
        Row::new(vec![
            r.group.clone(),
            r.id.clone(),
            r.state.clone(),
            r.users.join(", "),
            actions.join("  "),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Percentage(30),
            Constraint::Length(22),
            Constraint::Percentage(20),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(vec!["group", "id", "state", "used by", "actions"]).bold())
    .row_highlight_style(Style::default().reversed())
    .block(Block::bordered().title(" resources "));
    let mut state = TableState::default().with_selected(Some(view.selected));
    f.render_stateful_widget(table, table_area, &mut state);
    let text = match &view.pending {
        Some((id, action, plan)) => {
            format!("{action} {id}:\n{plan}\n\nEnter applies it, Esc discards it")
        }
        None => view.message.clone(),
    };
    f.render_widget(
        Paragraph::new(text).block(Block::bordered().title(" action ")),
        detail,
    );
}

fn draw_sensors(f: &mut Frame, area: Rect, readings: &[Reading]) {
    let rows = readings.iter().map(|r| {
        let value = r.value.map_or_else(
            || r.text.clone().unwrap_or_default(),
            |v| format!("{v:.1} {}", r.unit),
        );
        let low = r.history.iter().copied().reduce(f64::min);
        let high = r.history.iter().copied().reduce(f64::max);
        let range = low
            .zip(high)
            .filter(|_| r.history.len() > 1)
            .map_or_else(String::new, |(l, h)| {
                format!("{l:.1}-{h:.1} ({})", r.history.len())
            });
        Row::new(vec![r.source.clone(), r.name.clone(), value, range])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(26),
            Constraint::Percentage(35),
            Constraint::Percentage(30),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(vec!["source", "sensor", "value", "history"]).bold())
    .block(Block::bordered().title(" sensors "));
    f.render_widget(table, area);
}

/// The views that pane 3 can show: (view, key, icon, name), in the order of pane 1.
#[derive(Clone, Copy, PartialEq)]
enum ViewKind {
    Dashboard,
    Resources,
    Sensors,
    Output,
}

const VIEWS: [(ViewKind, char, &str, &str); 4] = [
    (ViewKind::Dashboard, 'd', "▦", "dashboard"),
    (ViewKind::Resources, 'r', "▣", "resources"),
    (ViewKind::Sensors, 's', "◎", "sensors"),
    (ViewKind::Output, 'o', "≡", "output"),
];

/// What the view of pane 3 read at its last refresh.
enum Data {
    Dashboard(Box<Snapshot>, Vec<Workspace>, Vec<ConfigFile>),
    Resources(ResourceList),
    Sensors(Vec<Reading>),
    Output,
}

/// A command that runs in the background: the operation and the line that started it (its row in pane 2).
struct Running {
    op: &'static str,
    row: usize,
    task: tokio::task::JoinHandle<Result<Value>>,
}

/// One `/` command that completion offers.
struct Entry {
    text: String,
    about: String,
    agent: bool,
}

#[derive(PartialEq)]
enum Focus {
    View,
    Chat,
}

struct Screen {
    client: Arc<Client>,
    view: ViewKind,
    data: Result<Data, String>,
    refreshed: Option<Instant>,
    resources: ResourcesState,
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

/// The rows of a thread's log for pane 2: time (UTC), member, kind and what it says.
fn log_line(row: &ThreadLogRow) -> String {
    let p = &row.payload;
    let detail = match (p["to"].as_str(), p["text"].as_str()) {
        (Some(to), Some(text)) => format!("→ {to}: {text}"),
        _ => ["title", "reason", "word", "archetype", "pace"]
            .iter()
            .find_map(|k| p[k].as_str())
            .map_or_else(|| p.to_string(), String::from),
    };
    format!(
        "{:02}:{:02} {} {} {detail}",
        row.at / 3600 % 24,
        row.at / 60 % 60,
        if row.member.is_empty() {
            "-"
        } else {
            &row.member
        },
        row.kind.trim_start_matches("thread."),
    )
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
    fn entries(&self) -> Vec<Entry> {
        let agent = self.commands.iter().map(|c| Entry {
            text: format!("/agent:{}", c.name),
            about: match &c.hint {
                Some(hint) => format!("{} <{hint}> (agent)", c.description),
                None => format!("{} (agent)", c.description),
            },
            agent: true,
        });
        let local =
            ["1", "2"]
                .iter()
                .map(|n| (format!("/fold {n}"), format!("Fold or open pane {n}")))
                .chain(VIEWS.iter().map(|(_, _, _, name)| {
                    (format!("/view {name}"), format!("Show {name} in pane 3"))
                }))
                .map(|(text, about)| Entry {
                    text,
                    about,
                    agent: false,
                });
        // an agent's call of a workspace operation waits for the user's approval
        let asks = crate::cli::session_from_env().is_some();
        let registry = OPS.iter().filter(|op| !op.internal()).map(|op| {
            let class = format!("{:?}", op.class).to_lowercase();
            let ask = if asks && op.class == Class::Workspace {
                ", asks the user"
            } else {
                ""
            };
            Entry {
                text: format!("/{}", op.name.replace('.', " ")),
                about: format!("{} [{class}{ask}]", op.summary()),
                agent: false,
            }
        });
        agent.chain(local).chain(registry).collect()
    }

    /// The entries that match the chat bar's text.
    fn completions(&self) -> Vec<Entry> {
        if self.focus != Focus::Chat || !self.chat.starts_with('/') || self.chat.contains('\n') {
            return Vec::new();
        }
        self.entries()
            .into_iter()
            .filter(|e| {
                e.text.starts_with(&self.chat) || self.chat.starts_with(&format!("{} ", e.text))
            })
            .collect()
    }

    fn refresh(&mut self, runtime: &tokio::runtime::Runtime) {
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
        let data = runtime.block_on(async {
            Ok(match self.view {
                ViewKind::Dashboard => {
                    let status = StatusIn {
                        profile: None,
                        fast: true,
                    };
                    let project = ProjectIn {
                        project: ClientPath(std::env::current_dir()?),
                    };
                    let (snapshot, configs, workspaces) = tokio::try_join!(
                        client.call::<op::Status>(&status),
                        client.call::<op::AgentsList>(&project),
                        client.call::<op::WorkspaceList>(&Empty {})
                    )?;
                    Data::Dashboard(Box::new(snapshot), workspaces, configs)
                }
                ViewKind::Resources => {
                    let list = client.call::<op::ResourcesList>(&Empty {}).await?;
                    self.resources.selected = self
                        .resources
                        .selected
                        .min(list.resources.len().saturating_sub(1));
                    Data::Resources(list)
                }
                ViewKind::Sensors => Data::Sensors(
                    client
                        .call::<op::Sensors>(&SensorsIn { watch: false })
                        .await?,
                ),
                ViewKind::Output => Data::Output,
            })
        });
        self.data = data.map_err(|e: anyhow::Error| format!("{e:#}"));
        self.refreshed = Some(Instant::now());
    }

    fn show(&mut self, view: ViewKind) {
        // the key of the view on screen goes back to the dashboard
        self.view = if self.view == view {
            ViewKind::Dashboard
        } else {
            view
        };
        self.refreshed = None;
    }

    /// Start `op` with `input` in the background, as the row `line` in pane 2.
    fn start(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        op: &'static str,
        input: Value,
        line: &str,
    ) {
        if self.running.is_some() {
            self.output = "a command runs: wait for its end".into();
            self.chat = line.to_string();
            return;
        }
        let client = self.client.clone();
        self.runs.push((line.to_string(), None));
        self.output = format!("running: {line}");
        self.view = ViewKind::Output;
        self.running = Some(Running {
            op,
            row: self.runs.len() - 1,
            task: runtime.spawn(async move { client.call_value(op, &input).await }),
        });
    }

    /// When the running command ended: its text in the output view, and a run's session attached.
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
                legionctl_api::render(running.op, value)
                    .unwrap_or_else(|e| (format!("{e:#}"), false))
            }
            Err(e) => (format!("{e:#}"), false),
        };
        self.output = text;
        self.runs[running.row].1 = Some(ok);
    }

    fn attach(&mut self, runtime: &tokio::runtime::Runtime, run: &RunOutput) {
        self.attached = Some((run.id.clone(), run.acp.clone()));
        let input = SessionIdIn { id: run.id.clone() };
        match runtime.block_on(self.client.call::<op::SessionCommands>(&input)) {
            Ok(commands) => self.commands = commands,
            Err(e) => self.output = format!("the agent's commands: {e:#}"),
        }
    }

    /// The chat bar's line: a command, or a prompt for the agent.
    fn submit(&mut self, runtime: &tokio::runtime::Runtime) {
        let line = std::mem::take(&mut self.chat).trim().to_string();
        if line.is_empty() {
            return;
        }
        let Some(command) = line.strip_prefix('/') else {
            return self.prompt(runtime, &line, &line);
        };
        let words: Vec<&str> = command.split_whitespace().collect();
        match words.as_slice() {
            ["fold", n @ ("1" | "2")] => {
                let i = usize::from(*n == "2");
                self.folded[i] = !self.folded[i];
            }
            ["view", name] => match VIEWS.iter().find(|v| v.3 == *name) {
                Some(v) => {
                    self.view = v.0;
                    self.refreshed = None;
                }
                None => self.output = format!("no view {name}"),
            },
            [first, ..] if first.starts_with("agent:") => {
                let name = &first["agent:".len()..];
                if self.commands.iter().any(|c| c.name == name) {
                    // ACP: a prompt that starts with `/<name>` runs the agent's command
                    self.prompt(runtime, &format!("/{}", &command["agent:".len()..]), &line);
                } else {
                    let known: Vec<String> = self
                        .commands
                        .iter()
                        .map(|c| format!("/agent:{}", c.name))
                        .collect();
                    self.output = if known.is_empty() {
                        format!(
                            "the agent offers no command {name}: it gives its commands in its first run, so send it a prompt first"
                        )
                    } else {
                        format!(
                            "the agent offers no command {name}; it offers {}",
                            known.join(", ")
                        )
                    };
                    self.view = ViewKind::Output;
                }
            }
            _ => self.operation(runtime, command, &line),
        }
    }

    /// An operation of the registry, with the CLI's words.
    fn operation(&mut self, runtime: &tokio::runtime::Runtime, command: &str, line: &str) {
        let parsed = shlex::split(command)
            .ok_or_else(|| anyhow::anyhow!("a quote is not closed"))
            .and_then(|words| {
                let matches = crate::cli::command(Vec::new())
                    .try_get_matches_from(std::iter::once("legionctl".to_string()).chain(words))
                    .map_err(|e| anyhow::anyhow!("{}", e.render()))?;
                crate::cli::operation(&matches, false)?
                    .ok_or_else(|| anyhow::anyhow!("not an operation: {command}"))
            });
        match parsed {
            Ok((op, input)) => self.start(runtime, op.name, input, line),
            Err(e) => {
                self.output = format!("{e:#}");
                self.view = ViewKind::Output;
            }
        }
    }

    /// `prompt` to the agent as a headless run that goes on in the agent's last ACP session.
    fn prompt(&mut self, runtime: &tokio::runtime::Runtime, prompt: &str, line: &str) {
        let Some(agent) = self.agent.clone() else {
            self.output =
                "no agent: start the screen with --agent <name> (config session.acp)".into();
            self.view = ViewKind::Output;
            return;
        };
        let repo = match std::env::current_dir() {
            Ok(dir) => dir,
            Err(e) => {
                self.output = format!("the working folder: {e}");
                return;
            }
        };
        let input = json!({
            "agent": agent,
            "prompt": prompt,
            "dossier": self.dossier,
            "repo": repo,
            "acp": self.attached.as_ref().and_then(|(_, acp)| acp.clone()),
        });
        self.start(runtime, "session.run", input, line);
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
        if let KeyCode::Char(c) = key.code
            && let Some(v) = VIEWS.iter().find(|v| v.1 == c)
        {
            self.show(v.0);
            return false;
        }
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('/') => {
                self.focus = Focus::Chat;
                self.chat = "/".into();
            }
            KeyCode::Tab => self.focus = Focus::Chat,
            _ if self.view == ViewKind::Resources => self.resources_key(runtime, key),
            _ => {}
        }
        false
    }

    fn resources_key(&mut self, runtime: &tokio::runtime::Runtime, key: KeyEvent) {
        let client = &self.client;
        let act = |id: &str, action: &str, dry_run: bool| {
            let input = ResourceActIn {
                id: id.into(),
                action: action.into(),
                dry_run,
            };
            runtime
                .block_on(client.call::<op::ResourcesAct>(&input))
                .map_or_else(|e| format!("{e:#}"), |acted| acted.text())
        };
        let view = &mut self.resources;
        match key.code {
            KeyCode::Up => view.selected = view.selected.saturating_sub(1),
            KeyCode::Down => view.selected += 1,
            KeyCode::Esc => view.pending = None,
            KeyCode::Enter => {
                if let Some((id, action, _)) = view.pending.take() {
                    view.message = act(&id, &action, false);
                }
            }
            KeyCode::Char(c @ '1'..='9') => {
                let Ok(Data::Resources(list)) = &self.data else {
                    return;
                };
                let chosen = list.resources.get(view.selected).and_then(|r| {
                    let i = c.to_digit(10)? as usize - 1;
                    Some((r.id.clone(), r.actions.get(i)?.name.clone()))
                });
                if let Some((id, action)) = chosen {
                    let plan = act(&id, &action, true);
                    view.pending = Some((id, action, plan));
                }
            }
            _ => {}
        }
        self.refreshed = None;
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
    let lines: Vec<Line> = VIEWS
        .iter()
        .map(|(view, key, icon, _)| {
            let line = Line::from(format!(" {icon} {key}"));
            if *view == s.view {
                line.reversed()
            } else {
                line
            }
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("1")),
        area,
    );
}

fn draw_events(f: &mut Frame, area: Rect, s: &Screen) {
    let (title, mut lines) = match &s.thread {
        Ok(Some(t)) => (
            format!(" 2 thread {}: {} ", t.thread.id, t.thread.title),
            t.log.iter().map(log_line).collect(),
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

fn draw_view(f: &mut Frame, area: Rect, s: &Screen) {
    match &s.data {
        Err(e) => {
            let name = VIEWS.iter().find(|v| v.0 == s.view).map_or("view", |v| v.3);
            f.render_widget(
                Paragraph::new(format!("cannot read the view: {e}"))
                    .block(Block::bordered().title(format!(" {name} "))),
                area,
            );
        }
        Ok(Data::Dashboard(snapshot, workspaces, configs)) => {
            draw_dashboard(f, area, snapshot, workspaces, configs);
        }
        Ok(Data::Resources(list)) => draw_resources(f, area, list, &s.resources),
        Ok(Data::Sensors(readings)) => draw_sensors(f, area, readings),
        Ok(Data::Output) => f.render_widget(
            Paragraph::new(s.output.as_str()).block(Block::bordered().title(" output ")),
            area,
        ),
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
    } else if s.view == ViewKind::Resources {
        " q quit · r back · Up/Down select · 1-9 an action (dry run first) · / command · Tab chat · Alt+1/Alt+2 fold"
    } else {
        " q quit · d dashboard · r resources · s sensors · o output · / command · Tab chat · Alt+1/Alt+2 fold"
    };
    f.render_widget(Paragraph::new(keys).dim(), help);
}

/// `legionctl tui [--thread <id>] [--agent <name>] [--dossier <name>]`.
pub fn run(args: &ArgMatches) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let mut screen = Screen {
        client: Arc::new(Client::new("tui")?),
        view: ViewKind::Dashboard,
        data: Ok(Data::Output),
        refreshed: None,
        resources: ResourcesState::default(),
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
            if screen
                .refreshed
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
            {
                screen.refresh(&runtime);
            }
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
