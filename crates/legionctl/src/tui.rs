//! Live dashboard: load pressure, machine state, workspaces and agent configs on one screen.

use std::time::Duration;

use anyhow::Result;
use legionctl_api::{ClientPath, ConfigFile, Empty, ProjectIn, Snapshot, StatusIn, Workspace, op};
use legionctl_machine::load::{Level, Pressure};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Gauge, Paragraph, Row, Table};

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

fn draw(f: &mut Frame, s: &Snapshot, workspaces: &[Workspace], configs: &[ConfigFile]) {
    let [header, gauges, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .areas(f.area());

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

    f.render_widget(
        Paragraph::new(" q quit · state refreshes every second").dim(),
        footer,
    );
}

pub fn run() -> Result<()> {
    let client = Client::new("tui")?;
    let runtime = tokio::runtime::Runtime::new()?;
    let status = StatusIn {
        profile: None,
        fast: true,
    };
    let project = ProjectIn {
        project: ClientPath(std::env::current_dir()?),
    };
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        loop {
            let (snapshot, configs, workspaces) = runtime.block_on(async {
                tokio::try_join!(
                    client.call::<op::Status>(&status),
                    client.call::<op::AgentsList>(&project),
                    client.call::<op::WorkspaceList>(&Empty {})
                )
            })?;
            terminal.draw(|f| draw(f, &snapshot, &workspaces, &configs))?;
            if event::poll(Duration::from_secs(1))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    _ => {}
                }
            }
        }
    })();
    ratatui::restore();
    result
}
