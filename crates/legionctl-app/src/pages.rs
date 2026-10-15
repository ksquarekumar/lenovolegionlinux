//! Pane 3's pages (Now, Apps & GPU, Health, Bench, Resources), the plan of a pending change, and the window around
//! the three panes. Each page reads `State`; each button runs an operation of `LegionApp`, its dry run first.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::chart::LineChart;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Sizable as _, StyledExt as _, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Window, div, px, relative,
};
use legionctl_api::{BenchRun, DesktopGpu, Reading, Render as _, Resource};

use crate::theme::{
    self, R_SM, S1, S2, S3, S5, T_LG, T_MD, T_SM, T_XL, T_XS, Tokens, badge, card, heading, icon,
};
use crate::{LegionApp, Page, Pending};

/// The pages of pane 1 with their icons.
pub(crate) const PAGES: [(Page, IconName); 5] = [
    (Page::Now, IconName::Activity),
    (Page::Gpu, IconName::Monitor),
    (Page::Health, IconName::HeartPulse),
    (Page::Bench, IconName::ChartLine),
    (Page::Resources, IconName::Boxes),
];

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
        Users
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

/// A value with its unit, or a dash.
fn number(v: Option<f64>, unit: &str) -> String {
    v.map_or("–".into(), |v| format!("{v:.0} {unit}"))
}

/// A line of history as a small chart; nothing for fewer than two values.
fn sparkline(id: &str, history: &[f64], color: Hsla) -> Option<AnyElement> {
    (history.len() > 1).then(|| {
        let points: Vec<(String, f64)> = history
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), *v))
            .collect();
        div()
            .h(px(36.))
            .w_full()
            .child(
                LineChart::new(points)
                    .id(SharedString::from(format!("spark-{id}")))
                    .x(|p: &(String, f64)| p.0.clone())
                    .y(|p: &(String, f64)| p.1)
                    .stroke(color)
                    .x_axis(false)
                    .y_axis(false)
                    .grid(false)
                    .interactive(false)
                    .appear(false),
            )
            .into_any_element()
    })
}

/// The page's title and what it shows.
fn page_header(title: &str, about: String, t: &Tokens) -> Div {
    div()
        .v_flex()
        .gap(S1)
        .child(
            div()
                .text_size(T_LG)
                .font_semibold()
                .text_color(t.text)
                .child(title.to_string()),
        )
        .child(div().text_size(T_SM).text_color(t.muted).child(about))
}

/// A row of a card: the label, then its value close to it.
fn row(label: String, value: String, color: Hsla, t: &Tokens) -> Div {
    div()
        .h_flex()
        .justify_between()
        .gap(S3)
        .py(px(3.))
        .text_size(T_SM)
        .items_start()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(t.text2)
                .truncate()
                .child(label),
        )
        .child(
            div()
                .max_w(relative(0.6))
                .text_right()
                .font_features(theme::tabular())
                .text_color(color)
                .child(value),
        )
}

/// The group of the Now page that a reading belongs to.
fn group_of(r: &Reading) -> String {
    let s = r.source.as_str();
    match s {
        "CPU" | "RAPL" | "cpufreq" => "CPU".into(),
        "nvidia-smi" | "PCI runtime PM" => "GPU".into(),
        "fans" | "firmware acpi_call" | "legionctl-system" => "Fans and firmware".into(),
        "battery" | "AC adapter" | "USB-C" | "power_supply" | "power_supply BAT0" => "Power".into(),
        _ if s.starts_with("NVMe") => s.into(),
        _ if s.starts_with("GPU") => "GPU".into(),
        _ if s.starts_with("memory") => "Memory".into(),
        _ => "System".into(),
    }
}

/// The temperature limits (warning, critical) of a group, in °C.
fn limits(group: &str) -> (f64, f64) {
    match group {
        "CPU" => (85., 95.),
        "GPU" => (80., 90.),
        g if g.starts_with("NVMe") => (65., 75.),
        _ => (75., 90.),
    }
}

fn group_icon(group: &str) -> IconName {
    match group {
        "CPU" => IconName::Cpu,
        "GPU" => IconName::Monitor,
        "Memory" => IconName::MemoryStick,
        "Fans and firmware" => IconName::Fan,
        "Power" => IconName::BatteryCharging,
        g if g.starts_with("NVMe") => IconName::HardDrive,
        _ => IconName::Gauge,
    }
}

/// A reading's value: the number with its unit, or its state.
fn value_of(r: &Reading) -> String {
    match r.value {
        Some(v) if r.unit == "°C" || r.unit == "RPM" || r.unit == "MHz" => {
            format!("{v:.0} {}", r.unit)
        }
        Some(v) => format!("{v:.1} {}", r.unit).trim_end().to_string(),
        None => r.text.clone().unwrap_or_default(),
    }
}

/// A hero card: label, value with unit, note, and the sparkline of its history.
fn hero_card(h: Hero, t: &Tokens) -> Div {
    card(t)
        .flex_1()
        .min_w(px(160.))
        .gap(S1)
        .child(
            div()
                .h_flex()
                .gap(S2)
                .text_size(T_XS)
                .text_color(t.muted)
                .child(icon(h.glyph, px(14.), t.muted))
                .child(h.label),
        )
        .child(
            div()
                .h_flex()
                .items_baseline()
                .gap(S1)
                .pt(S1)
                .child(
                    div()
                        .text_size(if h.value.len() > 8 { T_LG } else { T_XL })
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(!h.unit.is_empty(), |d| d.font_features(theme::tabular()))
                        .text_color(h.color)
                        .whitespace_nowrap()
                        .child(h.value),
                )
                .child(div().text_size(T_SM).text_color(t.muted).child(h.unit)),
        )
        .child(div().text_size(T_XS).text_color(t.muted).child(h.note))
        .children(sparkline(h.id, &h.history, h.color))
}

/// The CPU cores as a grid of small tiles, each with its temperature colour.
fn core_grid(cores: &[&Reading], color: impl Fn(&Reading) -> Hsla, t: &Tokens) -> Div {
    div()
        .v_flex()
        .child(
            div()
                .pt(S2)
                .text_size(T_XS)
                .text_color(t.muted)
                .child(format!("{} cores", cores.len())),
        )
        .child(
            div()
                .h_flex()
                .flex_wrap()
                .gap(S1)
                .pt(S1)
                .children(cores.iter().map(|r| {
                    div()
                        .w(px(58.))
                        .v_flex()
                        .items_center()
                        .py(S1)
                        .rounded(R_SM)
                        .bg(t.window)
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(t.muted)
                                .child(r.name.trim_start_matches("Core ").to_string()),
                        )
                        .child(
                            div()
                                .text_size(T_SM)
                                .font_features(theme::tabular())
                                .text_color(color(r))
                                .child(r.value.map_or_else(String::new, |v| format!("{v:.0}°"))),
                        )
                })),
        )
}

/// A sensor group's title: icon, name, the hottest value, the fold chevron.
fn group_title(
    name: &str,
    hottest: Option<(f64, Hsla)>,
    folded: bool,
    t: &Tokens,
) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("group-{name}")))
        .h_flex()
        .gap(S2)
        .cursor_pointer()
        .child(icon(group_icon(name), px(16.), t.accent))
        .child(
            div()
                .flex_1()
                .text_size(T_SM)
                .font_semibold()
                .text_color(t.text)
                .child(name.to_string()),
        )
        .children(hottest.map(|(h, color)| badge(format!("{h:.0} °C"), color)))
        .child(icon(
            if folded {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            },
            px(14.),
            t.muted,
        ))
}

/// One finding: its icon, what is wrong, and the hint ("what; see how", "what (how)") on its own line.
fn finding(line: &str, color: Hsla, glyph: IconName, t: &Tokens) -> Div {
    let (what, hint) = line
        .split_once("; ")
        .or_else(|| {
            line.split_once(" (")
                .map(|(a, b)| (a, b.trim_end_matches(')')))
        })
        .unwrap_or((line, ""));
    div()
        .h_flex()
        .items_start()
        .gap(S3)
        .py(S2)
        .child(div().pt(px(2.)).child(icon(glyph, px(16.), color)))
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(T_SM)
                        .text_color(t.text)
                        .child(what.to_string()),
                )
                .when(!hint.is_empty(), |d| {
                    d.child(
                        div()
                            .text_size(T_XS)
                            .text_color(t.muted)
                            .child(hint.to_string()),
                    )
                }),
        )
}

/// The score of a run whose key starts with `prefix` (`cpu bogo ops/s`, `gpu Gflop/s`).
fn score(r: &BenchRun, prefix: &str) -> Option<f64> {
    r.result
        .scores
        .iter()
        .find(|(k, _)| k.starts_with(prefix))
        .map(|(_, v)| *v)
}

/// The CPU score of each run over time, in the scores' own range so that a change of a few percent shows.
fn bench_chart(points: Vec<(String, f64)>, t: &Tokens) -> Div {
    let (low, high) = points
        .iter()
        .fold((f64::MAX, f64::MIN), |(l, h), p| (l.min(p.1), h.max(p.1)));
    let pad = ((high - low) * 0.2).max(high * 0.02);
    card(t)
        .child(
            div()
                .h_flex()
                .justify_between()
                .child(heading("CPU score over time", t))
                .child(
                    div()
                        .text_size(T_XS)
                        .text_color(t.muted)
                        .child(format!("{low:.0} to {high:.0} ops/s")),
                ),
        )
        .child(
            div().h(px(160.)).w_full().child(
                LineChart::new(points)
                    .id("bench-chart")
                    .x(|p: &(String, f64)| p.0.clone())
                    .y(|p: &(String, f64)| p.1)
                    .y_domain(low - pad, high + pad)
                    .stroke(t.accent)
                    .dot()
                    .tick_margin(4),
            ),
        )
}

/// The runs as a table, newest first.
fn bench_table(runs: &[BenchRun], t: &Tokens) -> Div {
    if runs.is_empty() {
        return card(t).child(
            div()
                .text_size(T_SM)
                .text_color(t.muted)
                .child("No run yet"),
        );
    }
    let columns = |cells: [String; 6], color: Hsla, weight: FontWeight| {
        div()
            .h_flex()
            .gap(S3)
            .py(S2)
            .border_b_1()
            .border_color(t.border)
            .text_size(T_SM)
            .text_color(color)
            .font_weight(weight)
            .font_features(theme::tabular())
            .children(cells.into_iter().enumerate().map(|(i, c)| {
                let cell = div().truncate().child(c);
                match i {
                    0 => cell.w(px(56.)),
                    1 => cell.flex_1().min_w(px(120.)),
                    5 => cell.w(px(110.)),
                    _ => cell.w(px(96.)).text_right(),
                }
            }))
    };
    let now = crate::thread::unix_now();
    // shortcut: the newest 200 rows; page the table if someone keeps more runs and wants to read them here
    card(t)
        .gap(px(0.))
        .child(columns(
            [
                "Run".into(),
                "Kind".into(),
                "CPU ops/s".into(),
                "GPU Gflop/s".into(),
                "Power".into(),
                "When".into(),
            ],
            t.muted,
            FontWeight::MEDIUM,
        ))
        .children(runs.iter().rev().take(200).map(|r| {
            columns(
                [
                    format!("#{}", r.id),
                    r.result
                        .label
                        .clone()
                        .unwrap_or_else(|| r.result.scenario.to_string()),
                    score(r, "cpu").map_or("–".into(), |v| format!("{v:.0}")),
                    score(r, "gpu").map_or("–".into(), |v| format!("{v:.0}")),
                    number(r.result.summary.package_w_mean, "W"),
                    crate::thread::ago(now, r.at),
                ],
                t.text,
                FontWeight::NORMAL,
            )
        }))
}

/// A hero card of the Now page.
struct Hero {
    id: &'static str,
    glyph: IconName,
    label: &'static str,
    value: String,
    unit: &'static str,
    note: String,
    color: Hsla,
    history: Vec<f64>,
}

impl LegionApp {
    /// The hero cards: firmware mode, CPU power and temperature, GPU power, fans; a sparkline from the sensors'
    /// history where the operator has one.
    fn hero(&self, t: &Tokens) -> Div {
        let status = self.state.status.as_ref();
        let sample = self.state.sample.as_ref();
        let readings = self.state.sensors.as_deref().unwrap_or_default();
        let history = |name: &str| -> Vec<f64> {
            readings
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.history.clone())
                .unwrap_or_default()
        };
        let why = match status {
            Some(s) if s.fn_q_override.is_some() => "your Fn+Q choice".to_string(),
            Some(s) => format!("on {}", s.power_source),
            None => String::new(),
        };
        let fans: Vec<String> = sample
            .and_then(|s| s.root.fan_rpm.as_ref())
            .map(|f| f.values().map(ToString::to_string).collect())
            .unwrap_or_default();
        let fan_note = match &self.state.fan {
            Some(f) if f.max => "max mode",
            Some(f) if f.mode.as_deref() == Some("custom") => "custom curve",
            Some(_) => "firmware curve",
            None => "",
        };
        let cpu_temp = sample.and_then(|s| s.cpu_temp_c);
        let fan_history: Vec<f64> = readings
            .iter()
            .find(|r| r.unit == "RPM")
            .map(|r| r.history.clone())
            .unwrap_or_default();
        let watts = |v: Option<f64>| v.map_or("–".into(), |v| format!("{v:.0}"));
        let cards = [
            Hero {
                id: "mode",
                glyph: IconName::Gauge,
                label: "Mode",
                value: status
                    .and_then(|s| s.platform_profile.clone())
                    .unwrap_or_else(|| "–".into()),
                unit: "",
                note: why,
                color: t.text,
                history: Vec::new(),
            },
            Hero {
                id: "cpu-power",
                glyph: IconName::Zap,
                label: "CPU power",
                value: watts(sample.and_then(|s| s.package_w)),
                unit: "W",
                note: "package".into(),
                color: t.text,
                history: history("CPU package power"),
            },
            Hero {
                id: "cpu-temp",
                glyph: IconName::Thermometer,
                label: "CPU temperature",
                value: watts(cpu_temp),
                unit: "°C",
                note: "package, warning at 85 °C".into(),
                color: cpu_temp.map_or(t.text, |v| t.level(v, 85., 95.)),
                history: history("Package id 0"),
            },
            Hero {
                id: "gpu-power",
                glyph: IconName::Monitor,
                label: "GPU power",
                value: watts(sample.and_then(|s| s.gpu.as_ref()?.power_w)),
                unit: "W",
                note: if sample.is_some_and(|s| s.gpu.is_none()) {
                    "asleep".into()
                } else {
                    "NVIDIA".into()
                },
                color: t.text,
                history: history("GPU power"),
            },
            Hero {
                id: "fans",
                glyph: IconName::Fan,
                label: "Fans",
                value: if fans.is_empty() {
                    "–".into()
                } else {
                    fans.join(" / ")
                },
                unit: "rpm",
                note: fan_note.into(),
                color: t.text,
                history: fan_history,
            },
        ];
        div()
            .h_flex()
            .flex_wrap()
            .items_stretch()
            .gap(S3)
            .children(cards.into_iter().map(|h| hero_card(h, t)))
    }

    /// The sensors in groups: CPU with a grid of its cores, GPU, each `NVMe` drive, memory, fans, power, the rest; a
    /// click on a group's title folds it.
    fn sensors(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let mut groups: Vec<(String, Vec<&Reading>)> = Vec::new();
        for r in self.state.sensors.iter().flatten() {
            let g = group_of(r);
            match groups.iter_mut().find(|(name, _)| *name == g) {
                Some((_, rows)) => rows.push(r),
                None => groups.push((g, vec![r])),
            }
        }
        let order = |g: &str| match g {
            "CPU" => 0,
            "GPU" => 1,
            g if g.starts_with("NVMe") => 2,
            "Memory" => 3,
            "Fans and firmware" => 4,
            "Power" => 5,
            _ => 6,
        };
        groups.sort_by_key(|(g, _)| order(g));
        div().v_flex().gap(S3).child(heading("Sensors", t)).child(
            div().h_flex().flex_wrap().items_start().gap(S3).children(
                groups
                    .into_iter()
                    .map(|(g, rows)| self.group(&g, &rows, t, cx)),
            ),
        )
    }

    fn group(&self, name: &str, rows: &[&Reading], t: &Tokens, cx: &mut Context<Self>) -> Div {
        let (warn, critical) = limits(name);
        let hottest = rows
            .iter()
            .filter(|r| r.unit == "°C")
            .filter_map(|r| r.value)
            .reduce(f64::max);
        let folded = self.state.folded_groups.contains(name);
        let key = name.to_string();
        let title = group_title(
            name,
            hottest.map(|h| (h, t.level(h, warn, critical))),
            folded,
            t,
        )
        .on_click(cx.listener(move |app, _, _, cx| {
            if !app.state.folded_groups.remove(&key) {
                app.state.folded_groups.insert(key.clone());
            }
            cx.notify();
        }));
        let color = |r: &Reading| match (r.value, r.unit.as_str()) {
            (Some(v), "°C") => t.level(v, warn, critical),
            (Some(_), _) => t.text,
            (None, _) => t.text2,
        };
        let (cores, rest): (Vec<&Reading>, Vec<&Reading>) = rows
            .iter()
            .partition(|r| name == "CPU" && r.name.starts_with("Core "));
        let mut cores = cores;
        cores.sort_by_key(|r| {
            r.name
                .trim_start_matches("Core ")
                .parse::<u32>()
                .unwrap_or(u32::MAX)
        });
        // a group of one device names the reading; a group of several names the device too
        let single = name == "CPU" || name == "GPU" || name.starts_with("NVMe");
        let label = |r: &Reading| {
            if single
                || ["legionctl-system", "firmware acpi_call", "fans"].contains(&r.source.as_str())
            {
                r.name.clone()
            } else {
                format!("{} · {}", r.source.trim_start_matches("memory "), r.name)
            }
        };
        let body = div()
            .v_flex()
            .children(rest.iter().map(|r| row(label(r), value_of(r), color(r), t)))
            .when(!cores.is_empty(), |body| {
                body.child(core_grid(&cores, color, t))
            });
        card(t)
            .flex_1()
            .min_w(px(300.))
            .max_w(px(520.))
            .gap(S2)
            .child(title)
            .when(!folded, |c| c.child(body))
    }

    fn now_page(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        div()
            .v_flex()
            .gap(S5)
            .child(page_header(
                "Now",
                "The machine at a glance; the values refresh every 2 s".into(),
                t,
            ))
            .child(self.hero(t))
            .child(
                div().h_flex().child(
                    Button::new("preview-policy")
                        .icon(IconName::Wrench)
                        .label("Check the power policy")
                        .loading(self.state.busy)
                        .on_click(cx.listener(|app, _, _, cx| app.preview_policy(cx))),
                ),
            )
            .child(self.sensors(t, cx))
    }

    /// The desktop GPU as a segmented control that shows the choice now, and the displays.
    fn gpu_page(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        const CHOICES: [(DesktopGpu, &str); 3] = [
            (DesktopGpu::Nvidia, "NVIDIA"),
            (DesktopGpu::Intel, "Intel"),
            (DesktopGpu::Auto, "Let GNOME choose"),
        ];
        let gpu = self.state.gpu.as_ref();
        let current = gpu.and_then(|g| CHOICES.iter().position(|(c, _)| *c == g.desktop_gpu));
        let chooser = card(t)
            .child(heading("The GPU that draws the desktop", t))
            .child(
                div()
                    .text_size(T_SM)
                    .text_color(t.muted)
                    .child("GNOME reads it at the next login. A choice shows its plan first."),
            )
            .child(
                div().h_flex().child(
                    TabBar::new("desktop-gpu")
                        .segmented()
                        .when_some(current, TabBar::selected_index)
                        .children(CHOICES.iter().map(|(_, label)| Tab::new().label(*label)))
                        .on_click(cx.listener(|app, index: &usize, _, cx| {
                            if let Some((choice, _)) = CHOICES.get(*index) {
                                app.preview_desktop_gpu(*choice, cx);
                            }
                        })),
                ),
            );
        let displays = card(t)
            .child(
                div()
                    .h_flex()
                    .gap(S2)
                    .child(heading("Displays", t))
                    .children(gpu.map(|g| badge(g.mode.to_string(), t.accent))),
            )
            .children(match gpu {
                None => vec![div().text_size(T_SM).text_color(t.muted).child("reading…")],
                Some(g) => g
                    .outputs
                    .iter()
                    .filter(|o| o.connected)
                    .map(|o| {
                        div()
                            .h_flex()
                            .gap(S3)
                            .text_size(T_SM)
                            .child(icon(IconName::Monitor, px(16.), t.muted))
                            .child(
                                div()
                                    .w(px(80.))
                                    .text_color(t.text)
                                    .child(o.connector.clone()),
                            )
                            .child(div().text_color(t.text2).child(o.card.clone()))
                            .child(badge(o.driver.clone(), t.muted))
                    })
                    .collect(),
            });
        div()
            .v_flex()
            .gap(S5)
            .child(page_header(
                "Apps & GPU",
                gpu.map_or_else(String::new, |g| {
                    format!("The desktop is drawn on {} now", g.desktop_gpu)
                }),
                t,
            ))
            .child(chooser)
            .child(displays)
    }

    /// Doctor's findings by severity; the setup findings are one finding with a count and the `setup apply` button.
    fn health_page(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let page = div().v_flex().gap(S5);
        let Some(findings) = &self.state.findings else {
            return page.child(page_header("Health", "Checking…".into(), t));
        };
        let setup: Vec<String> = findings
            .iter()
            .filter_map(|f| f.strip_prefix("setup: "))
            .map(|f| {
                f.trim_end_matches("; run legionctl setup apply")
                    .to_string()
            })
            .collect();
        let (critical, warn): (Vec<&String>, Vec<&String>) = findings
            .iter()
            .filter(|f| !f.starts_with("setup: "))
            .partition(|f| f.contains("failed") || f.contains("error"));
        let summary = match (critical.len(), warn.len() + usize::from(!setup.is_empty())) {
            (0, 0) => "No problems found".to_string(),
            (c, w) => format!("{c} critical, {w} to check"),
        };
        let mut page = page.child(page_header("Health", summary, t));
        if findings.is_empty() {
            return page.child(
                card(t).child(
                    div()
                        .h_flex()
                        .gap(S2)
                        .child(icon(IconName::CircleCheck, px(18.), t.ok))
                        .child(
                            div()
                                .text_size(T_MD)
                                .child("The machine is as its profile says"),
                        ),
                ),
            );
        }
        for (title, lines, color, glyph) in [
            ("Critical", &critical, t.critical, IconName::CircleX),
            ("To check", &warn, t.warn, IconName::TriangleAlert),
        ] {
            if !lines.is_empty() {
                page = page.child(
                    card(t)
                        .gap(px(0.))
                        .child(
                            div()
                                .h_flex()
                                .gap(S2)
                                .pb(S1)
                                .child(heading(title, t))
                                .child(badge(lines.len().to_string(), color)),
                        )
                        .children(lines.iter().map(|l| finding(l, color, glyph, t))),
                );
            }
        }
        if !setup.is_empty() {
            page = page.child(self.setup_card(&setup, t, cx));
        }
        page
    }

    /// The missing setup items as one finding: their count, the list on demand, and Apply (`setup apply`).
    fn setup_card(&self, setup: &[String], t: &Tokens, cx: &mut Context<Self>) -> Div {
        let open = self.state.setup_open;
        let items = setup.to_vec();
        card(t)
            .child(
                div()
                    .h_flex()
                    .gap(S3)
                    .child(icon(IconName::Wrench, px(16.), t.warn))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .child(
                                div().text_size(T_SM).text_color(t.text).child(format!(
                                    "{} agent setup items are missing",
                                    setup.len()
                                )),
                            )
                            .child(
                                div()
                                    .text_size(T_XS)
                                    .text_color(t.muted)
                                    .child("Apply runs legionctl setup apply"),
                            ),
                    )
                    .child(
                        Button::new("setup-list")
                            .ghost()
                            .label(if open { "Hide" } else { "Show" })
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.state.setup_open = !app.state.setup_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("setup-apply")
                            .primary()
                            .label("Apply")
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.preview_setup(items.clone(), cx);
                            })),
                    ),
            )
            .when(open, |c| {
                c.child(div().v_flex().pl(px(28.)).children(setup.iter().map(|s| {
                    div()
                        .py(px(2.))
                        .text_size(T_XS)
                        .text_color(t.text2)
                        .child(s.clone())
                })))
            })
    }

    /// The runs as a table, newest first, and the CPU score over time.
    fn bench_page(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let runs: &[BenchRun] = self.state.runs.as_deref().unwrap_or_default();
        let header = div()
            .h_flex()
            .justify_between()
            .items_end()
            .child(page_header(
                "Bench",
                format!("{} runs in the event log", runs.len()),
                t,
            ))
            .child(
                Button::new("bench-cpu")
                    .primary()
                    .icon(IconName::Play)
                    .label("Run a 30 s CPU bench")
                    .loading(self.state.busy)
                    .on_click(cx.listener(|app, _, _, cx| app.bench(30, cx))),
            );
        let points: Vec<(String, f64)> = runs
            .iter()
            .filter_map(|r| Some((format!("#{}", r.id), score(r, "cpu")?)))
            .collect();
        let chart = (points.len() > 1).then(|| bench_chart(points, t));
        div()
            .v_flex()
            .gap(S5)
            .child(header)
            .children(chart)
            .child(bench_table(runs, t))
    }

    /// One table per group: the resource, its state, what uses it (a count, the list in its tooltip) and the
    /// actions of its state.
    fn resources_page(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let page = div().v_flex().gap(S5);
        let Some(list) = &self.state.resources else {
            return page.child(page_header("Resources", "Reading the resources…".into(), t));
        };
        let mut groups: Vec<(&str, Vec<&Resource>)> = Vec::new();
        for r in &list.resources {
            match groups.iter_mut().find(|(g, _)| *g == r.group) {
                Some((_, rows)) => rows.push(r),
                None => groups.push((&r.group, vec![r])),
            }
        }
        let title = |g: &str| match g {
            "vm" => "Virtual machines and containers".to_string(),
            "gpu" => "GPUs".into(),
            "disk" => "Disks".into(),
            "configset" => "Configuration layers".into(),
            "session" => "Agent sessions".into(),
            other => other.to_string(),
        };
        page.child(page_header(
            "Resources",
            format!(
                "{} resources; an action shows its plan first",
                list.resources.len()
            ),
            t,
        ))
        .children(groups.into_iter().map(|(g, rows)| {
            card(t)
                .gap(px(0.))
                .child(div().pb(S2).child(heading(title(g), t)))
                .children(rows.into_iter().map(|r| Self::resource_row(r, t, cx)))
        }))
    }

    /// A resource's row: name and id, state, users (a count; the list in the tooltip), the actions of its state.
    fn resource_row(r: &Resource, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let users = r.users.join("\n");
        let state = if r.state.starts_with("running")
            || r.state.starts_with("mounted")
            || r.state.starts_with("on")
            || r.state == "linux"
        {
            t.ok
        } else if r.state.starts_with("paused") || r.state.starts_with("held") {
            t.warn
        } else {
            t.muted
        };
        div()
            .h_flex()
            .gap(S3)
            .py(S2)
            .border_t_1()
            .border_color(t.border)
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w(px(160.))
                    .child(
                        div()
                            .text_size(T_SM)
                            .text_color(t.text)
                            .child(r.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(T_XS)
                            .text_color(t.muted)
                            .truncate()
                            .child(r.id.clone()),
                    ),
            )
            .child(
                div()
                    .w(px(130.))
                    .flex_none()
                    .h_flex()
                    .child(badge(r.state.clone(), state)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("users-{}", r.id)))
                    .w(px(100.))
                    .flex_none()
                    .h_flex()
                    .gap(S1)
                    .text_size(T_XS)
                    .text_color(t.muted)
                    .when(!r.users.is_empty(), |d| {
                        d.child(icon(IconName::Users, px(13.), t.muted))
                            .child(match r.users.len() {
                                1 => div().truncate().child(r.users[0].clone()),
                                n => div().child(format!("{n} users")),
                            })
                            .tooltip(move |window, cx| {
                                Tooltip::new(users.clone()).build(window, cx)
                            })
                    }),
            )
            .child(
                div()
                    .w(px(220.))
                    .flex_none()
                    .h_flex()
                    .justify_end()
                    .gap(S1)
                    .children(r.actions.iter().map(|a| {
                        let (id, action) = (r.id.clone(), a.name.clone());
                        Button::new(SharedString::from(format!("{}/{}", r.id, a.name)))
                            .small()
                            .outline()
                            .label(a.name.clone())
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.preview_resource(id.clone(), action.clone(), cx);
                            }))
                    })),
            )
    }

    /// The plan of the pending change, with Apply and Discard.
    fn pending(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<Div> {
        let pending = self.state.pending.as_ref()?;
        let (title, lines) = match pending {
            Pending::Policy(c) => ("The power policy would change".to_string(), c.lines.clone()),
            Pending::DesktopGpu(_, c) => (
                "The desktop GPU choice would change (next login)".to_string(),
                c.lines.clone(),
            ),
            Pending::Resource(id, action, acted) => (
                format!("{action} {id} runs {}", acted.op),
                acted.text().lines().map(String::from).collect(),
            ),
            Pending::Setup(items) => (
                "Setup would install what is missing".to_string(),
                items.clone(),
            ),
        };
        // the buttons first: a long plan must not push them out of reach
        let buttons = div()
            .h_flex()
            .gap(S2)
            .child(
                Button::new("apply")
                    .primary()
                    .label("Apply")
                    .disabled(lines.is_empty())
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
                        .child(heading(title, t))
                        .child(buttons),
                )
                .child(
                    div()
                        .v_flex()
                        .gap(px(2.))
                        .text_size(T_SM)
                        .text_color(t.text2)
                        .children(if lines.is_empty() {
                            vec![div().child("Nothing: the machine is already in this state")]
                        } else {
                            lines.iter().map(|l| div().child(l.clone())).collect()
                        }),
                ),
        )
    }

    fn page(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        match self.page {
            Page::Now => self.now_page(t, cx),
            Page::Gpu => self.gpu_page(t, cx),
            Page::Health => self.health_page(t, cx),
            Page::Bench => self.bench_page(t, cx),
            Page::Resources => self.resources_page(t, cx),
        }
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
        let place = match &self.screen.view {
            Some(shown) => shown.name.clone(),
            None => self.page.title().to_string(),
        };
        let title = TitleBar::new().child(
            div()
                .h_flex()
                .gap(S2)
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
                .child(div().text_color(t.text2).child(place)),
        );
        let mut body = div().v_flex().gap(S5);
        if let Some(why) = &self.state.offline {
            body = body.child(LegionApp::offline(why, &t));
        } else {
            body = body
                .children(self.pending(&t, cx))
                .child(if self.screen.view.is_some() {
                    self.view(cx)
                } else {
                    self.page(&t, cx)
                });
            if let Some(message) = &self.state.message {
                body = body.child(
                    card(&t)
                        .gap(S1)
                        .text_size(T_SM)
                        .text_color(t.text2)
                        .child(
                            div()
                                .h_flex()
                                .gap(S2)
                                .text_color(t.muted)
                                .text_size(T_XS)
                                .child(icon(IconName::Info, px(14.), t.muted))
                                .child("Last result"),
                        )
                        .children(message.lines().map(|l| div().child(l.to_string()))),
                );
            }
        }
        div()
            .size_full()
            .v_flex()
            .bg(t.window)
            .text_color(t.text)
            .text_size(T_SM)
            .on_key_down(cx.listener(LegionApp::key))
            .child(title)
            .child(self.panes(body, &t, window, cx))
    }
}
