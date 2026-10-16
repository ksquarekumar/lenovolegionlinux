//! `legionctl-app` (note 09): the machine at a glance with every sensor (note 16), GPU wiring, health, bench and the
//! resources (note 22), from `legionctl-operator`.
//! Every value comes from an operation; every change is an operation that shows its plan first (a dry run), and the
//! daemon asks polkit when it is applied.

pub mod greeter;
mod pages;
pub use pages::AppAssets;
pub mod theme;
pub mod thread;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{AppContext, AsyncApp, Context, FocusHandle, Task, WeakEntity};
use legionctl_api::client::Client;
use legionctl_api::{
    Acted, BenchIn, BenchRun, Changes, DesktopGpu, Empty, FanState, Findings, GpuPrimaryIn,
    GpuState, MonitorIn, PageIn, PowerApplyIn, PowerSource, ProfileIn, Reading, Render as _,
    ResourceActIn, ResourceList, Sample, Scenario, SensorsIn, SetupApplyIn, Snapshot, StatusIn, op,
};

/// How often the glance values refresh.
const REFRESH: Duration = Duration::from_secs(2);
/// How often the thread screen takes the events that the stream gave.
const EVENTS: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Now,
    Gpu,
    Health,
    Bench,
    Resources,
}

impl Page {
    pub const ALL: [Page; 5] = [
        Page::Now,
        Page::Gpu,
        Page::Health,
        Page::Bench,
        Page::Resources,
    ];

    /// The page a command-line word names: now, gpu, health, bench, resources.
    #[must_use]
    pub fn named(word: &str) -> Option<Page> {
        Page::ALL
            .into_iter()
            .find(|p| format!("{p:?}").eq_ignore_ascii_case(word))
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Page::Now => "Now",
            Page::Gpu => "Apps & GPU",
            Page::Health => "Health",
            Page::Bench => "Bench",
            Page::Resources => "Resources",
        }
    }
}

/// A change waiting for the user: the plan the operator showed, and how to apply it.
#[derive(Debug, Clone)]
pub enum Pending {
    Policy(Changes),
    DesktopGpu(DesktopGpu, Changes),
    /// A resource's action (id, action): its dry run, or its route when its operation has no dry run.
    Resource(String, String, Acted),
    /// `setup apply` for the setup findings it fixes (it has no dry run: the findings are its plan).
    Setup(Vec<String>),
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
    pub resources: Option<ResourceList>,
    pub sensors: Option<Vec<Reading>>,
    pub pending: Option<Pending>,
    /// The last result or error, in one line.
    pub message: Option<String>,
    /// The operator does not answer: the app shows how to start it.
    pub offline: Option<String>,
    pub busy: bool,
    /// The sensor groups the user folded.
    pub folded_groups: BTreeSet<String>,
    /// The list of the missing setup items is open.
    pub setup_open: bool,
}

pub struct LegionApp {
    client: Arc<Client>,
    runtime: tokio::runtime::Handle,
    pub page: Page,
    pub state: State,
    /// The thread screen: panes, chat bar, thread and view.
    pub screen: thread::Screen,
    chat_focus: FocusHandle,
    _refresh: Task<()>,
    _events: Task<()>,
    _follower: Follower,
}

/// The task that follows the event stream; it stops with the app.
struct Follower(tokio::task::JoinHandle<()>);

impl Drop for Follower {
    fn drop(&mut self) {
        self.0.abort();
    }
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
        let client = Arc::new(client);
        let (events, follower) = LegionApp::follow(client.clone(), &runtime, cx);
        let mut app = LegionApp {
            client,
            runtime,
            page,
            state: State::default(),
            screen: thread::Screen::new(std::env::current_dir().unwrap_or_else(|_| "/".into())),
            chat_focus: cx.focus_handle(),
            _refresh: refresh,
            _events: events,
            _follower: follower,
        };
        app.read_thread(cx);
        app
    }

    /// The dossier of the chat bar's runs and their repository.
    #[must_use]
    pub fn with_runs(mut self, dossier: Option<String>, repo: std::path::PathBuf) -> Self {
        self.screen.dossier = dossier;
        self.screen.repo = repo;
        self
    }

    /// Follow the operator's event stream from now on the app's tokio runtime; the kinds of the events go to
    /// [`LegionApp::events`]. A broken stream opens again after its last event (C6).
    fn follow(
        client: Arc<Client>,
        runtime: &tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> (Task<()>, Follower) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let follower = runtime.spawn(async move {
            let mut offset = "now".to_string();
            loop {
                if let Ok(events) = client.follow(&offset).await {
                    let _ = events
                        .each(|e| {
                            if let Some(id) = e["id"].as_i64() {
                                offset = id.to_string();
                            }
                            let _ = tx.send(e["kind"].as_str().unwrap_or_default().to_string());
                        })
                        .await;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        // shortcut: the screen takes the events each 100 ms, because GPUI's test scheduler refuses a wake from a
        // tokio thread; wake it from the stream if GPUI gets a thread-safe test waker
        let task = cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                loop {
                    cx.background_executor().timer(EVENTS).await;
                    let mut kinds = Vec::new();
                    while let Ok(kind) = rx.try_recv() {
                        kinds.push(kind);
                    }
                    if this
                        .update(cx, |app, cx| {
                            if !kinds.is_empty() {
                                app.events(&kinds, cx);
                            }
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            },
        );
        (task, Follower(follower))
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
            Page::Now => {
                let task = self.operator(cx, async move {
                    client
                        .call::<op::Sensors>(&SensorsIn { watch: false })
                        .await
                });
                self.finish(cx, task, |app, readings| app.state.sensors = Some(readings));
            }
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
            Page::Resources => {
                let task = self.operator(cx, async move {
                    client.call::<op::ResourcesList>(&Empty {}).await
                });
                self.finish(cx, task, |app, list| app.state.resources = Some(list));
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
        self.screen.view = None;
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
                    release_nvidia: false,
                    dry_run: true,
                })
                .await
        });
        self.finish(cx, task, move |app, changes| {
            app.state.pending = Some(Pending::DesktopGpu(choice, changes));
        });
    }

    /// What the action `action` of the resource `id` would do: its operation's dry run, or its route.
    pub fn preview_resource(&mut self, id: String, action: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let input = ResourceActIn {
            id: id.clone(),
            action: action.clone(),
            dry_run: true,
        };
        let task = self.operator(
            cx,
            async move { client.call::<op::ResourcesAct>(&input).await },
        );
        self.finish(cx, task, move |app, acted| {
            app.state.pending = Some(Pending::Resource(id, action, acted));
        });
    }

    /// Apply the pending change; the daemon asks polkit.
    pub fn apply(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.state.pending.take() else {
            return;
        };
        let client = self.client.clone();
        let changed = |changes: Changes| {
            if changes.lines.is_empty() {
                "nothing to change".to_string()
            } else {
                changes.lines.join("; ")
            }
        };
        let task = self.operator(cx, async move {
            Ok(match pending {
                Pending::Policy(_) => changed(
                    client
                        .call::<op::PowerApply>(&PowerApplyIn {
                            source: PowerSource::Auto,
                            dry_run: false,
                        })
                        .await?,
                ),
                Pending::DesktopGpu(choice, _) => changed(
                    client
                        .call::<op::GpuPrimary>(&GpuPrimaryIn {
                            choice,
                            release_nvidia: false,
                            dry_run: false,
                        })
                        .await?,
                ),
                Pending::Setup(_) => changed(
                    client
                        .call::<op::SetupApply>(&SetupApplyIn { install_ctx: false })
                        .await?,
                ),
                Pending::Resource(id, action, _) => client
                    .call::<op::ResourcesAct>(&ResourceActIn {
                        id,
                        action,
                        dry_run: false,
                    })
                    .await?
                    .text(),
            })
        });
        self.finish(cx, task, |app, message| app.state.message = Some(message));
    }

    /// The setup findings as the plan of `setup apply`.
    pub fn preview_setup(&mut self, items: Vec<String>, cx: &mut Context<Self>) {
        self.state.pending = Some(Pending::Setup(items));
        cx.notify();
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
