//! `legionctl-app` (note 09): the machine at a glance, GPU wiring, health and bench, from `legionctl-operator`.
//! Every value comes from an operation; every change is an operation that shows its plan first (a dry run), and the
//! daemon asks polkit when it is applied.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::TitleBar;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, StyledExt};
use gpui_kit::{
    App, AppContext, AsyncApp, Context, Div, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Task, WeakEntity, Window, div, px,
};
use legionctl_api::client::Client;
use legionctl_api::{
    BenchIn, BenchRun, Changes, DesktopGpu, Empty, FanState, Findings, GpuPrimaryIn, GpuState,
    MonitorIn, PageIn, PowerApplyIn, PowerSource, ProfileIn, Sample, Scenario, Snapshot, StatusIn,
    op,
};

/// How often the glance values refresh.
const REFRESH: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Now,
    Gpu,
    Health,
    Bench,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::Now, Page::Gpu, Page::Health, Page::Bench];

    /// The page a command-line word names: now, gpu, health, bench.
    #[must_use]
    pub fn named(word: &str) -> Option<Page> {
        Page::ALL
            .into_iter()
            .find(|p| format!("{p:?}").eq_ignore_ascii_case(word))
    }

    fn title(self) -> &'static str {
        match self {
            Page::Now => "Now",
            Page::Gpu => "Apps & GPU",
            Page::Health => "Health",
            Page::Bench => "Bench",
        }
    }
}

/// A change waiting for the user: the plan the operator showed, and how to apply it.
#[derive(Debug, Clone)]
pub enum Pending {
    Policy(Changes),
    DesktopGpu(DesktopGpu, Changes),
}

/// What the app knows; each field is `None` until its first answer.
#[derive(Debug, Default)]
pub struct State {
    pub status: Option<Snapshot>,
    pub sample: Option<Sample>,
    /// `None` also when the daemon cannot say (it is not running).
    pub fan: Option<FanState>,
    pub gpu: Option<GpuState>,
    pub findings: Option<Vec<String>>,
    pub runs: Option<Vec<BenchRun>>,
    pub pending: Option<Pending>,
    /// The last result or error, in one line.
    pub message: Option<String>,
    /// The operator does not answer: the app shows how to start it.
    pub offline: Option<String>,
    pub busy: bool,
}

pub struct LegionApp {
    client: Arc<Client>,
    runtime: tokio::runtime::Handle,
    pub page: Page,
    pub state: State,
    _refresh: Task<()>,
}

impl LegionApp {
    pub fn new(
        client: Client,
        runtime: tokio::runtime::Handle,
        page: Page,
        cx: &mut Context<Self>,
    ) -> LegionApp {
        let refresh = cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                loop {
                    if this.update(cx, LegionApp::refresh).is_err() {
                        return;
                    }
                    cx.background_executor().timer(REFRESH).await;
                }
            },
        );
        LegionApp {
            client: Arc::new(client),
            runtime,
            page,
            state: State::default(),
            _refresh: refresh,
        }
    }

    /// The glance values, and the data of the open page.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            let status = client
                .call::<op::Status>(&StatusIn {
                    profile: None,
                    fast: true,
                })
                .await?;
            let sample = client
                .call::<op::Monitor>(&MonitorIn {
                    count: 1,
                    interval_ms: 500,
                })
                .await?
                .pop();
            let fan = client.call::<op::FanStatus>(&Empty {}).await.ok();
            anyhow::Ok((status, sample, fan))
        });
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| {
                    match result {
                        Ok((status, sample, fan)) => {
                            app.state.status = Some(status);
                            app.state.sample = sample;
                            app.state.fan = fan;
                            app.state.offline = None;
                        }
                        Err(e) => app.state.offline = Some(format!("{e:#}")),
                    }
                    cx.notify();
                });
            },
        )
        .detach();
        self.load_page(cx);
    }

    fn load_page(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        match self.page {
            Page::Now => {}
            Page::Gpu => {
                let task =
                    self.operator(
                        cx,
                        async move { client.call::<op::GpuStatus>(&Empty {}).await },
                    );
                self.finish(cx, task, |app, gpu| app.state.gpu = Some(gpu));
            }
            Page::Health => {
                let task = self.operator(cx, async move {
                    client
                        .call::<op::Doctor>(&ProfileIn { profile: None })
                        .await
                });
                self.finish(cx, task, |app, f: Findings| {
                    app.state.findings = Some(f.findings);
                });
            }
            Page::Bench => {
                // every page, so the newest runs are there too
                let task = self.operator(cx, async move {
                    let mut runs = Vec::new();
                    let mut cursor = None;
                    loop {
                        let page = client
                            .call::<op::BenchList>(&PageIn {
                                cursor,
                                limit: 1000,
                            })
                            .await?;
                        runs.extend(page.items);
                        match page.next {
                            Some(next) => cursor = Some(next),
                            None => break Ok(runs),
                        }
                    }
                });
                self.finish(cx, task, |app, runs| app.state.runs = Some(runs));
            }
        }
    }

    /// Run an operator call. It blocks a background thread of GPUI's executor on the app's tokio runtime, so that
    /// only GPUI's own threads wake the view's tasks.
    fn operator<T: Send + 'static>(
        &self,
        cx: &Context<Self>,
        call: impl Future<Output = anyhow::Result<T>> + Send + 'static,
    ) -> Task<anyhow::Result<T>> {
        let runtime = self.runtime.clone();
        cx.background_spawn(async move { runtime.block_on(call) })
    }

    /// Await an operator call on the view: `ok` with the answer, or the error as the message.
    fn finish<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        task: Task<anyhow::Result<T>>,
        ok: impl FnOnce(&mut LegionApp, T) + 'static,
    ) {
        self.state.busy = true;
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| {
                    app.state.busy = false;
                    match result {
                        Ok(value) => ok(app, value),
                        Err(e) => app.state.message = Some(format!("{e:#}")),
                    }
                    cx.notify();
                });
            },
        )
        .detach();
    }

    pub fn open(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        self.load_page(cx);
        cx.notify();
    }

    /// What the power policy would change now (dry run).
    pub fn preview_policy(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            client
                .call::<op::PowerApply>(&PowerApplyIn {
                    source: PowerSource::Auto,
                    dry_run: true,
                })
                .await
        });
        self.finish(cx, task, |app, changes| {
            app.state.pending = Some(Pending::Policy(changes));
        });
    }

    /// What choosing `choice` for the desktop GPU would change (dry run).
    pub fn preview_desktop_gpu(&mut self, choice: DesktopGpu, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            client
                .call::<op::GpuPrimary>(&GpuPrimaryIn {
                    choice,
                    dry_run: true,
                })
                .await
        });
        self.finish(cx, task, move |app, changes| {
            app.state.pending = Some(Pending::DesktopGpu(choice, changes));
        });
    }

    /// Apply the pending change; the daemon asks polkit.
    pub fn apply(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.state.pending.take() else {
            return;
        };
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            match pending {
                Pending::Policy(_) => {
                    client
                        .call::<op::PowerApply>(&PowerApplyIn {
                            source: PowerSource::Auto,
                            dry_run: false,
                        })
                        .await
                }
                Pending::DesktopGpu(choice, _) => {
                    client
                        .call::<op::GpuPrimary>(&GpuPrimaryIn {
                            choice,
                            dry_run: false,
                        })
                        .await
                }
            }
        });
        self.finish(cx, task, |app, changes: Changes| {
            app.state.message = Some(if changes.lines.is_empty() {
                "nothing to change".into()
            } else {
                changes.lines.join("; ")
            });
        });
    }

    pub fn discard(&mut self, cx: &mut Context<Self>) {
        self.state.pending = None;
        cx.notify();
    }

    /// A short CPU bench; the list refreshes after it.
    pub fn bench(&mut self, seconds: u32, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            client
                .call::<op::BenchRunOp>(&BenchIn {
                    scenario: Scenario::Cpu,
                    seconds,
                    label: Some("app".into()),
                })
                .await
        });
        self.finish(cx, task, |app, run: BenchRun| {
            app.state.message = Some(format!("bench run {} done", run.id));
            app.state.runs.get_or_insert_with(Vec::new).push(run);
        });
    }
}

fn number(v: Option<f64>, unit: &str) -> String {
    v.map_or("–".into(), |v| format!("{v:.0} {unit}"))
}

fn tile(label: &str, value: String, cx: &App) -> Div {
    div()
        .v_flex()
        .gap_1()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .min_w(px(120.))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label.to_string()),
        )
        .child(div().text_lg().font_semibold().child(value))
}

impl LegionApp {
    fn glance(&self, cx: &App) -> Div {
        let status = self.state.status.as_ref();
        let sample = self.state.sample.as_ref();
        let mode = status
            .and_then(|s| s.platform_profile.clone())
            .unwrap_or_else(|| "–".into());
        let why = match status {
            Some(s) if s.fn_q_override.is_some() => "your Fn+Q choice".to_string(),
            Some(s) => format!("on {}", s.power_source),
            None => String::new(),
        };
        let fans = sample
            .and_then(|s| s.root.fan_rpm.as_ref())
            .map_or("–".into(), |f| {
                f.values()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" / ")
            });
        div()
            .h_flex()
            .flex_wrap()
            .gap_2()
            .child(tile("Mode", format!("{mode} ({why})"), cx))
            .child(tile(
                "CPU power",
                number(sample.and_then(|s| s.package_w), "W"),
                cx,
            ))
            .child(tile(
                "CPU temperature",
                number(sample.and_then(|s| s.cpu_temp_c), "°C"),
                cx,
            ))
            .child(tile(
                "GPU power",
                number(sample.and_then(|s| s.gpu.as_ref()?.power_w), "W"),
                cx,
            ))
            .child(tile(
                "Fans",
                match &self.state.fan {
                    Some(f) if f.max => format!("{fans} rpm, max mode"),
                    Some(f) if f.mode.as_deref() == Some("custom") => {
                        format!("{fans} rpm, custom curve")
                    }
                    _ => format!("{fans} rpm"),
                },
                cx,
            ))
    }

    fn pending(&self, cx: &mut Context<Self>) -> Option<Div> {
        let pending = self.state.pending.as_ref()?;
        let (title, lines) = match pending {
            Pending::Policy(c) => ("The power policy would change:", &c.lines),
            Pending::DesktopGpu(_, c) => (
                "The desktop GPU choice would change (next login):",
                &c.lines,
            ),
        };
        // the buttons first: a long plan must not push them out of reach
        let buttons = div()
            .h_flex()
            .gap_2()
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
            div()
                .v_flex()
                .gap_2()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .child(div().font_semibold().child(title))
                .child(buttons)
                .children(if lines.is_empty() {
                    vec![div().child("nothing: the machine is already in this state")]
                } else {
                    lines
                        .iter()
                        .map(|l| div().text_sm().child(l.clone()))
                        .collect()
                }),
        )
    }

    fn page(&self, cx: &mut Context<Self>) -> Div {
        match self.page {
            Page::Now => div().v_flex().gap_3().child(self.glance(cx)).child(
                Button::new("preview-policy")
                    .label("Check the power policy")
                    .loading(self.state.busy)
                    .on_click(cx.listener(|app, _, _, cx| app.preview_policy(cx))),
            ),
            Page::Gpu => {
                let mut page = div().v_flex().gap_2();
                if let Some(gpu) = &self.state.gpu {
                    page = page
                        .child(format!(
                            "Displays: {} · desktop drawn on {}",
                            gpu.mode, gpu.desktop_gpu
                        ))
                        .children(gpu.outputs.iter().filter(|o| o.connected).map(|o| {
                            div()
                                .text_sm()
                                .child(format!("{} on {} ({})", o.connector, o.card, o.driver))
                        }));
                }
                page.child(
                    div().h_flex().gap_2().children(
                        [
                            (
                                "gpu-nvidia",
                                "Draw the desktop on NVIDIA",
                                DesktopGpu::Nvidia,
                            ),
                            ("gpu-intel", "Draw the desktop on Intel", DesktopGpu::Intel),
                            ("gpu-auto", "Let GNOME choose", DesktopGpu::Auto),
                        ]
                        .map(|(id, label, choice)| {
                            let current = self
                                .state
                                .gpu
                                .as_ref()
                                .is_some_and(|g| g.desktop_gpu == choice);
                            Button::new(id)
                                .label(label)
                                .selected(current)
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    app.preview_desktop_gpu(choice, cx);
                                }))
                        }),
                    ),
                )
            }
            Page::Health => div().v_flex().gap_2().children(match &self.state.findings {
                None => vec![div().child("checking…")],
                Some(f) if f.is_empty() => {
                    vec![div().child(Tag::success().child("No problems found"))]
                }
                Some(f) => f
                    .iter()
                    .map(|line| div().text_sm().child(format!("• {line}")))
                    .collect(),
            }),
            Page::Bench => div()
                .v_flex()
                .gap_2()
                .child(
                    Button::new("bench-cpu")
                        .label("Run a 30 s CPU bench")
                        .loading(self.state.busy)
                        .on_click(cx.listener(|app, _, _, cx| app.bench(30, cx))),
                )
                .children(self.state.runs.iter().flatten().rev().map(|r| {
                    let scores: Vec<String> = r
                        .result
                        .scores
                        .iter()
                        .map(|(k, v)| format!("{k} {v:.0}"))
                        .collect();
                    div().text_sm().child(format!(
                        "run {} · {} · {} · {}",
                        r.id,
                        r.result.label.clone().unwrap_or_default(),
                        scores.join(", "),
                        number(r.result.summary.package_w_mean, "W")
                    ))
                })),
        }
    }
}

impl Render for LegionApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = TitleBar::new().child(div().font_semibold().child("legionctl"));
        // the page scrolls: a small window must still reach Apply
        let mut body = div()
            .id("body")
            .flex_1()
            .overflow_y_scroll()
            .v_flex()
            .gap_3()
            .p_4();
        if let Some(why) = &self.state.offline {
            body = body
                .child(
                    div()
                        .font_semibold()
                        .child("legionctl-operator does not answer"),
                )
                .child(div().text_sm().child(why.clone()))
                .child(
                    div()
                        .text_sm()
                        .child("Start it: systemctl --user enable --now legionctl-operator.socket"),
                );
        } else {
            let selected = Page::ALL.iter().position(|p| *p == self.page).unwrap_or(0);
            body = body
                .child(
                    TabBar::new("pages")
                        .selected_index(selected)
                        .children(Page::ALL.map(|p| Tab::new().label(p.title())))
                        .on_click(
                            cx.listener(|app, ix: &usize, _, cx| app.open(Page::ALL[*ix], cx)),
                        ),
                )
                .child(self.page(cx))
                .children(self.pending(cx));
            if let Some(message) = &self.state.message {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message.clone()),
                );
            }
        }
        div()
            .size_full()
            .v_flex()
            .bg(cx.theme().background)
            .child(title)
            .child(body)
    }
}
