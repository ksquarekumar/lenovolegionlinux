//! The Activity space (DESIGN-09): approvals that wait for the user, failures (doctor's critical findings), running
//! jobs, and the events since the app started, newest first.

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::{AnyElement, Context, IntoElement, StatefulInteractiveElement as _};
use legionctl_api::{EventRow, Job, PageIn, ProfileIn, op};

use crate::LegionApp;
use crate::theme::Tokens;

/// The newest events the screen keeps.
const KEPT: usize = 50;

#[derive(Debug, Default)]
pub struct Activity {
    /// The events since the app started, newest last.
    pub events: Vec<EventRow>,
    pub jobs: Vec<Job>,
    /// doctor's findings that are failures; none until the first answer.
    pub failures: Option<Vec<String>>,
}

impl Activity {
    /// Approvals asked of the user and not answered yet: an `approval.ask` with no later `approval.answer` of the
    /// same session and request.
    #[must_use]
    pub fn approvals(&self) -> Vec<&EventRow> {
        // shortcut: only asks seen since the app started; read the log back if an ask outlives an app restart
        let mut open: Vec<&EventRow> = Vec::new();
        for e in &self.events {
            match e.kind.as_str() {
                "approval.ask" => open.push(e),
                "approval.answer" => open.retain(|a| {
                    a.payload["session"] != e.payload["session"]
                        || a.payload["what"] != e.payload["what"]
                }),
                _ => {}
            }
        }
        open
    }

    /// What needs the user: open approvals and failures (the rail's dot).
    #[must_use]
    pub fn attention(&self) -> usize {
        self.approvals().len() + self.failures.as_ref().map_or(0, Vec::len)
    }

    pub fn take(&mut self, rows: Vec<EventRow>) {
        self.events.extend(rows);
        let extra = self.events.len().saturating_sub(KEPT);
        self.events.drain(..extra);
    }
}

/// A failure among doctor's findings: what failed or cannot run, not a setup item or a note.
fn failure(finding: &str) -> bool {
    !finding.starts_with("setup: ")
        && (finding.contains("failed") || finding.contains("error") || finding.contains("cannot"))
}

impl LegionApp {
    /// The jobs that run and doctor's failures (the Activity space, when it shows).
    pub(crate) fn load_activity(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            // shortcut: pages through every job for the running ones; add a state filter to jobs.list when the
            // table grows past a few thousand rows
            let mut running = Vec::new();
            let mut cursor = None;
            loop {
                let page = client
                    .call::<op::JobsList>(&PageIn {
                        cursor,
                        limit: 1000,
                    })
                    .await?;
                running.extend(page.items.into_iter().filter(|j| j.state == "running"));
                match page.next {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            let findings = client
                .call::<op::Doctor>(&ProfileIn { profile: None })
                .await?
                .findings;
            anyhow::Ok((running, findings))
        });
        self.finish(cx, task, |app, (jobs, findings)| {
            app.screen.activity.jobs = jobs;
            app.screen.activity.failures = Some(
                findings
                    .into_iter()
                    .map(|f| f.text)
                    .filter(|f| failure(f))
                    .collect(),
            );
        });
    }

    pub(crate) fn activity_entries(&self, t: &Tokens, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let a = &self.screen.activity;
        let mut out = vec![Self::nav_section("Waiting for you", t)];
        let approvals = a.approvals();
        if approvals.is_empty() {
            out.push(Self::nav_note("No approval waits", t));
        }
        out.extend(approvals.into_iter().map(|e| {
            Self::nav_entry(
                format!("approval-{}", e.id),
                (
                    IconName::TriangleAlert,
                    e.payload["what"].as_str().unwrap_or_default().to_string(),
                    Some(("asks".into(), t.warn)),
                ),
                false,
                t,
            )
            .into_any_element()
        }));
        out.push(Self::nav_section("Failures", t));
        match &a.failures {
            None => out.push(Self::nav_note("Checking…", t)),
            Some(f) if f.is_empty() => out.push(Self::nav_note("No failure", t)),
            Some(f) => out.extend(f.iter().enumerate().map(|(i, line)| {
                Self::nav_entry(
                    format!("failure-{i}"),
                    (IconName::CircleX, line.clone(), None),
                    false,
                    t,
                )
                .on_click(cx.listener(|app, _, _, cx| app.open_view("health", cx)))
                .test_support()
                .into_any_element()
            })),
        }
        out.push(Self::nav_section("Running jobs", t));
        if a.jobs.is_empty() {
            out.push(Self::nav_note("No job runs", t));
        }
        out.extend(a.jobs.iter().map(|j| {
            Self::nav_entry(
                format!("job-{}", j.id),
                (
                    IconName::LoaderCircle,
                    format!("{} #{}", j.kind, j.id),
                    None,
                ),
                false,
                t,
            )
            .into_any_element()
        }));
        out.push(Self::nav_section("Events since the start", t));
        if a.events.is_empty() {
            out.push(Self::nav_note("No event yet", t));
        }
        let now = crate::thread::unix_now();
        out.extend(a.events.iter().rev().map(|e| {
            Self::nav_entry(
                format!("event-{}", e.id),
                (
                    IconName::Activity,
                    format!("{} {}", e.actor, e.kind),
                    Some((crate::thread::ago(now, e.at), t.muted)),
                ),
                false,
                t,
            )
            .into_any_element()
        }));
        out
    }
}
