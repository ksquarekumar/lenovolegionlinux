//! Terminal text for each output type. JSON surfaces (MCP, `--json`) do not use it.

use std::collections::BTreeMap;

use crate::io::{
    CapabilityList, Changes, ConfigFile, ConfigView, DossierFindings, DossierInfo, EventRow,
    Explanation, Findings, GpuState, Guard, HookReply, HookTable, Launch, LayerList, Lines,
    LoadState, Lock, Reminder, RunEnv, Sample, SessionState, SetupReport, SkillInfo, Snapshot,
    Text, Validation, Workspace, WorkspaceStatus,
};

pub trait Render {
    fn text(&self) -> String;

    /// A CLI exits 1 when this is false (doctor findings, an invalid profile).
    fn success(&self) -> bool {
        true
    }
}

fn counts(map: &BTreeMap<String, usize>) -> String {
    let parts: Vec<String> = map.iter().map(|(k, n)| format!("{k}×{n}")).collect();
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(" ")
    }
}

fn or_unknown(map: &BTreeMap<String, Option<String>>, key: &str) -> String {
    map.get(key)
        .cloned()
        .flatten()
        .unwrap_or_else(|| "?".into())
}

impl Render for Snapshot {
    fn text(&self) -> String {
        let s = self;
        let mut out = vec![format!(
            "profile {} · DMI {} · BIOS {} ({}) · EC {}",
            s.profile_id,
            if s.dmi_mismatches.is_empty() {
                "match"
            } else {
                "MISMATCH"
            },
            or_unknown(&s.firmware_versions, "bios_version"),
            or_unknown(&s.firmware_versions, "bios_release"),
            or_unknown(&s.firmware_versions, "ec_firmware_release")
        )];
        out.extend(s.dmi_mismatches.iter().map(|m| {
            format!(
                "  {}: expected '{}', got {}",
                m.key,
                m.expected,
                m.actual
                    .as_deref()
                    .map_or("none".into(), |a| format!("'{a}'"))
            )
        }));
        out.push(format!(
            "power: {}  mode: {}{}  (choices: {})",
            s.power_source,
            s.platform_profile.as_deref().unwrap_or("?"),
            s.fn_q_override
                .as_ref()
                .map_or(String::new(), |o| format!(" (Fn+Q: {o})")),
            s.platform_profile_choices.join(" ")
        ));
        out.push(format!(
            "cpu: governor {}  EPP {}  no_turbo {}  max_perf_pct {}  ignore_ppc {}",
            counts(&s.governors),
            counts(&s.energy_performance_preference),
            or_unknown(&s.cpu_settings, "no_turbo"),
            or_unknown(&s.cpu_settings, "max_perf_pct"),
            or_unknown(&s.cpu_settings, "ignore_ppc")
        ));
        let limits: Vec<String> = s
            .firmware_limits
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        out.push(format!("firmware limits: {}", limits.join(" ")));
        out.extend(s.rapl.iter().map(|zone| {
            let parts: Vec<String> = zone
                .constraints
                .iter()
                .map(|c| {
                    format!(
                        "{} {}",
                        c.name,
                        c.limit_w.map_or("?".into(), |w| format!("{w:.0} W"))
                    )
                })
                .collect();
            format!("rapl {}: {}", zone.zone, parts.join(", "))
        }));
        out.push(format!(
            "temp: {}  fans: {}  throttle logged: {}",
            s.package_temp_c
                .map_or("?".into(), |t| format!("{t:.0} °C")),
            s.fan_rpm
                .as_ref()
                .map_or("unknown (needs root)".into(), |f| {
                    f.values()
                        .map(|r| format!("{r} rpm"))
                        .collect::<Vec<_>>()
                        .join(" / ")
                }),
            s.throttle_reasons
                .as_ref()
                .map_or("unknown (needs root)".into(), |r| {
                    if r.is_empty() {
                        "none".into()
                    } else {
                        r.join(", ")
                    }
                })
        ));
        out.extend(system_lines(s));
        out.join("\n")
    }
}

/// GPU wiring, units, modules and load: the second half of the status text.
fn system_lines(s: &Snapshot) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!(
        "gpu: {} · desktop on {} · NVIDIA limit {}",
        s.gpu_mode,
        s.desktop_gpu,
        s.nvidia_power_limit_w
            .as_deref()
            .map_or("?".into(), |w| format!("{w} W"))
    ));
    out.extend(
        s.outputs
            .iter()
            .filter(|o| o.connected)
            .map(|o| format!("  {} on {} ({})", o.connector, o.card, o.driver)),
    );
    out.extend(
        s.units
            .iter()
            .map(|u| format!("unit {}: {} / {}", u.unit, u.active, u.enabled)),
    );
    let modules: Vec<String> = s
        .modules
        .iter()
        .map(|(m, loaded)| format!("{m}{}", if *loaded { "" } else { " (not loaded)" }))
        .collect();
    out.push(format!("modules: {}", modules.join(", ")));
    out.push(legionctl_machine::load::brief(&s.load));
    out
}

impl Render for LoadState {
    fn text(&self) -> String {
        legionctl_machine::load::brief(self)
    }
}

impl Render for Findings {
    fn text(&self) -> String {
        if self.findings.is_empty() {
            "doctor: no obvious conflicts".into()
        } else {
            let lines: String = self.findings.iter().flat_map(|f| ["\n- ", f]).collect();
            format!("doctor: warnings{lines}")
        }
    }

    fn success(&self) -> bool {
        self.findings.is_empty()
    }
}

impl Render for CapabilityList {
    fn text(&self) -> String {
        let or = |v: &Option<String>| v.clone().unwrap_or_else(|| "unknown".into());
        let mut out = vec![format!("profile: {}", self.profile_id)];
        for c in &self.capabilities {
            out.push(format!("{}: {}", c.id, c.name));
            out.push(format!("  status: {}", or(&c.status)));
            out.push(format!("  current_owner: {}", or(&c.current_owner)));
            out.push(format!("  planned_owner: {}", or(&c.planned_owner)));
            if let Some(notes) = &c.notes {
                out.push(format!("  notes: {notes}"));
            }
        }
        out.join("\n")
    }
}

impl Render for Validation {
    fn text(&self) -> String {
        let lines: String = self.problems.iter().flat_map(|p| ["- ", p, "\n"]).collect();
        format!(
            "{lines}validate: {} ({})",
            if self.success() { "ok" } else { "failed" },
            self.path
        )
    }

    fn success(&self) -> bool {
        self.problems.is_empty()
    }
}

impl Render for GpuState {
    fn text(&self) -> String {
        let mut out = vec![
            format!("gpu_mode: {}", self.mode),
            format!(
                "desktop GPU preference: {} (Mutter reads it at login)",
                self.desktop_gpu
            ),
        ];
        out.extend(self.outputs.iter().map(|o| {
            format!(
                "  {}: {} ({}) {}",
                o.connector,
                o.card,
                o.driver,
                if o.connected { "connected" } else { "-" }
            )
        }));
        out.join("\n")
    }
}

impl Render for RunEnv {
    fn text(&self) -> String {
        let mut out: Vec<String> = self.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        out.push(format!(
            "AllowedCPUs={}",
            self.cpus.as_deref().unwrap_or("all")
        ));
        out.join("\n")
    }
}

impl Render for Changes {
    fn text(&self) -> String {
        if self.lines.is_empty() {
            if self.dry_run {
                "no changes needed"
            } else {
                "no changes"
            }
            .into()
        } else {
            self.lines.join("\n")
        }
    }
}

impl Render for Vec<ConfigFile> {
    fn text(&self) -> String {
        self.iter()
            .map(|f| {
                let state = match (f.exists, f.valid) {
                    (false, _) => "missing".to_string(),
                    (true, Some(true)) => format!("valid [{}]", f.sections.join(", ")),
                    (true, _) => format!("INVALID: {}", f.error.clone().unwrap_or_default()),
                };
                format!(
                    "{:<7} {:<14} {}  {}",
                    f.agent,
                    f.scope,
                    f.path.display(),
                    state
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for Workspace {
    fn text(&self) -> String {
        format!(
            "{}: {} on {} (base {}; main checkout {})",
            self.dossier,
            self.path.display(),
            self.branch,
            self.base,
            self.repo.display()
        )
    }
}

impl Render for Vec<Workspace> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no open workspaces".into();
        }
        self.iter().map(Render::text).collect::<Vec<_>>().join("\n")
    }
}

impl Render for Vec<WorkspaceStatus> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "this dossier has no open workspace".into();
        }
        let mut out = Vec::new();
        for s in self {
            out.push(s.workspace.text());
            out.push(format!(
                "  {} ahead, {} behind; staged {}, unstaged {}, untracked {}, conflicts {}",
                s.ahead, s.behind, s.staged, s.unstaged, s.untracked, s.conflicted
            ));
            out.extend(s.drift.iter().map(|d| format!("  drift: {d}")));
        }
        out.join("\n")
    }

    fn success(&self) -> bool {
        self.iter().all(|s| s.drift.is_empty())
    }
}

impl Render for SetupReport {
    fn text(&self) -> String {
        let mut out = vec![format!(
            "ctx: {}",
            self.ctx_version.as_deref().unwrap_or("not installed")
        )];
        out.extend(
            self.items
                .iter()
                .map(|i| format!("{:<26} {:<12} {:<14} {}", i.what, i.agent, i.status, i.path)),
        );
        let todo = self.needs_attention().len();
        out.push(if todo == 0 {
            "all set up".into()
        } else {
            format!("{todo} items need attention; run `legionctl setup apply`")
        });
        out.join("\n")
    }

    fn success(&self) -> bool {
        self.needs_attention().is_empty()
    }
}

impl Render for Vec<Reminder> {
    fn text(&self) -> String {
        self.iter()
            .map(|r| {
                format!(
                    "{:<3} {:<20} {:<14} {:<9} {}",
                    if r.enabled { "on" } else { "off" },
                    r.id,
                    serde_json::to_value(r.event)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default(),
                    if r.builtin { "built-in" } else { "custom" },
                    r.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for Text {
    fn text(&self) -> String {
        self.text.clone()
    }
}

impl Render for HookTable {
    fn text(&self) -> String {
        let mut out = vec!["bindings (in order):".to_string()];
        out.extend(self.bindings.iter().map(|b| {
            let event = serde_json::to_value(b.event)
                .ok()
                .and_then(|v| v.as_str().map(String::from));
            format!("  {:<14} {}", event.unwrap_or_default(), b.action)
        }));
        out.push("actions:".into());
        out.extend(self.actions.iter().map(|a| {
            let kind = if a.builtin {
                "built-in".to_string()
            } else {
                format!("command, {}s", a.timeout_s)
            };
            format!("  {:<16} {:<14} {}", a.id, kind, a.detail)
        }));
        out.join("\n")
    }
}

impl Render for Vec<SkillInfo> {
    fn text(&self) -> String {
        let mut out = Vec::new();
        for s in self {
            let reminder = match (&s.reminder, s.reminder_on) {
                (Some(id), Some(true)) => format!("reminder {id}: on"),
                (Some(id), _) => format!("reminder {id}: off"),
                _ => "no reminder".into(),
            };
            out.push(format!("legionctl:{}  ({reminder})", s.name));
            out.push(format!(
                "  claude: {}",
                s.claude
                    .as_ref()
                    .map_or("plugin not installed".into(), |p| p.display().to_string())
            ));
            out.push(format!(
                "  codex:  {} ({})",
                s.codex.display(),
                if s.codex_linked {
                    "linked"
                } else {
                    "missing; run `legionctl setup apply`"
                }
            ));
        }
        out.join("\n")
    }
}

fn problems(lines: &mut Vec<String>, problems: &[String]) {
    if !problems.is_empty() {
        lines.push("not used:".into());
        lines.extend(problems.iter().map(|p| format!("  {p}")));
    }
}

impl Render for ConfigView {
    fn text(&self) -> String {
        let mut out = vec![serde_json::to_string_pretty(&self.value).unwrap_or_default()];
        problems(&mut out, &self.problems);
        out.join("\n")
    }

    fn success(&self) -> bool {
        self.problems.is_empty()
    }
}

impl Render for Explanation {
    fn text(&self) -> String {
        let mut out = vec![format!(
            "{} = {}{}",
            self.key,
            self.value,
            if self.locked {
                "  (locked by /etc/legionctl)"
            } else {
                ""
            }
        )];
        out.extend(self.settings.iter().map(|(key, s)| {
            format!(
                "  {:<8} {}  {key} = {}",
                s.source.layer, s.source.file, s.value
            )
        }));
        out.join("\n")
    }
}

impl Render for LayerList {
    fn text(&self) -> String {
        let mut out: Vec<String> = self
            .layers
            .iter()
            .map(|l| {
                let files = if l.files.is_empty() {
                    "no files".into()
                } else {
                    l.files.join(", ")
                };
                format!(
                    "{:<8} {}  ({files})",
                    l.name,
                    l.dir.as_deref().unwrap_or("built in")
                )
            })
            .collect();
        if !self.locked.is_empty() {
            out.push(format!("locked: {}", self.locked.join(", ")));
        }
        problems(&mut out, &self.problems);
        out.join("\n")
    }

    fn success(&self) -> bool {
        self.problems.is_empty()
    }
}

impl Render for HookReply {
    fn text(&self) -> String {
        self.block.clone().unwrap_or_else(|| self.text.clone())
    }

    fn success(&self) -> bool {
        self.block.is_none()
    }
}

impl Render for Lines {
    fn text(&self) -> String {
        self.lines.join("\n")
    }
}

impl Render for Vec<Lock> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no dossier is active in any session".into();
        }
        self.iter()
            .map(|l| {
                format!(
                    "{}  session={}  tool={}  updated={}{}",
                    l.dossier,
                    l.session,
                    l.tool,
                    l.updated,
                    if l.stale { "  STALE" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for Vec<DossierInfo> {
    fn text(&self) -> String {
        self.iter()
            .map(|d| {
                if d.has_plan {
                    d.name.clone()
                } else {
                    format!("{}  (no plan.md)", d.name)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for DossierFindings {
    fn text(&self) -> String {
        if self.problems.is_empty() {
            format!("{}/plan.md has every ledger section", self.dossier)
        } else {
            self.problems.join("\n")
        }
    }

    fn success(&self) -> bool {
        self.problems.is_empty()
    }
}

impl Render for Guard {
    fn text(&self) -> String {
        self.refusal.clone().unwrap_or_else(|| "allowed".into())
    }

    fn success(&self) -> bool {
        self.refusal.is_none()
    }
}

impl Render for Launch {
    fn text(&self) -> String {
        format!(
            "session {}: {}",
            self.id,
            [&self.scope[..], &self.sandbox[..], &self.agent[..]]
                .concat()
                .join(" ")
        )
    }
}

impl Render for Vec<SessionState> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no sessions".into();
        }
        self.iter()
            .map(|s| {
                format!(
                    "{}  {:<8} {:<7} {:<16} {}",
                    s.session.id,
                    s.state,
                    s.session.agent,
                    s.session.dossier,
                    s.session.workdir.display()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn some(v: Option<f64>, unit: &str) -> String {
    v.map_or("-".into(), |v| format!("{v:.0}{unit}"))
}

impl Render for Vec<Sample> {
    fn text(&self) -> String {
        self.iter()
            .map(|s| {
                let gpu = s.gpu.as_ref().map_or("-".into(), |g| {
                    format!("{} {} {}", g.state, some(g.power_w, " W"), some(g.temp_c, " °C"))
                });
                let fans = s.root.fan_rpm.as_ref().map_or("? (root)".into(), |f| {
                    f.values().map(ToString::to_string).collect::<Vec<_>>().join("/")
                });
                let throttle = s.root.throttle_now.as_ref().map_or("? (root)".into(), |t| {
                    if t.is_empty() { "-".into() } else { t.join(",") }
                });
                format!(
                    "pkg {}  cpu {} {}  gpu {gpu}  fans {fans} rpm  throttle {throttle}  psi cpu/mem/io {}/{}/{}",
                    some(s.package_w, " W"),
                    some(s.cpu_temp_c, " °C"),
                    some(s.cpu_mhz, " MHz"),
                    some(s.cpu_pressure.as_ref().map(|p| p.some_avg10), "%"),
                    some(s.memory_pressure.as_ref().map(|p| p.some_avg10), "%"),
                    some(s.io_pressure.as_ref().map(|p| p.some_avg10), "%"),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for Vec<EventRow> {
    fn text(&self) -> String {
        self.iter()
            .map(|e| {
                format!(
                    "{:>6} {} {:<16} {:<28} {}",
                    e.id, e.ts, e.actor, e.kind, e.payload
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
