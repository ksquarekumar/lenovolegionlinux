//! Terminal text for each output type. JSON surfaces (MCP, `--json`) do not use it.

use std::collections::BTreeMap;

use crate::{
    BatteryState, BenchCompare, BenchRun, BootState, CapabilityList, Changes, Conditions,
    ConfigFile, ConfigView, DossierFindings, DossierInfo, Empty, EventRow, Explanation, Findings,
    GpuState, GreeterState, Guard, HookReply, HookTable, Knob, KnobDetail, Launch, LayerList,
    Lines, LoadState, Lock, Manifest, PackageState, PlanView, Recording, Reminder, RuleDetail,
    RuleState, Rules, RunEnv, Sample, SessionState, SetupReport, SkillInfo, Snapshot, ToolCallRow,
    TuneReport, Validation, VfioDevices, WindowsState, WindowsVmBegun, Workspace, WorkspaceStatus,
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
    out.push(s.load.text());
    out
}

impl Render for LoadState {
    fn text(&self) -> String {
        legionctl_machine::load::brief_of(
            self.level.into(),
            &self.reasons,
            (self.load1, self.cpus),
            (self.mem_available_bytes, self.mem_total_bytes),
            self.io_stall_source.as_deref(),
        )
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

impl Render for BatteryState {
    fn text(&self) -> String {
        let b = &self.battery;
        let or = |v: Option<String>| v.unwrap_or_else(|| "unknown".into());
        let mut out = vec![
            format!(
                "mode: {}  {} at {}%",
                or(b.mode.clone()),
                or(b.status.clone()),
                or(b.capacity_pct.map(|c| c.to_string()))
            ),
            format!(
                "health: {}% ({} of {} Wh), {} cycles",
                or(b.health_pct.map(|h| format!("{h:.1}"))),
                or(b.energy_full_wh.map(|e| format!("{e:.1}"))),
                or(b.energy_full_design_wh.map(|e| format!("{e:.1}"))),
                or(b.cycle_count.map(|c| c.to_string()))
            ),
            format!(
                "USB charging while off: {}",
                b.usb_charging
                    .map_or("unknown", |on| if on { "on" } else { "off" })
            ),
        ];
        if let (Some(first), Some(last)) = (self.health_trend.first(), self.health_trend.last())
            && self.health_trend.len() > 1
        {
            let days = (last.0 - first.0) / 86_400;
            out.push(format!(
                "health trend: {:.1}% -> {:.1}% in {days} days ({} samples)",
                first.1,
                last.1,
                self.health_trend.len()
            ));
        }
        out.join("\n")
    }
}

impl Render for BootState {
    fn text(&self) -> String {
        let label = |id: &str| {
            self.entries
                .iter()
                .find(|e| e.id == id)
                .map_or_else(|| id.to_string(), |e| format!("{id} {}", e.label))
        };
        let mut out = vec![
            format!(
                "this start: {}",
                self.current.as_deref().map_or("unknown".into(), label)
            ),
            format!(
                "next start: {}",
                self.next
                    .as_deref()
                    .map_or("the first in the order".into(), label)
            ),
            format!(
                "order: {}",
                self.order
                    .iter()
                    .map(|id| label(id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            match &self.ours {
                Some(id) => format!("legionctl's entry: {}", label(id)),
                None => "legionctl's entry: none (legionctl boot install)".into(),
            },
            format!("ESP: {}", self.esp.as_deref().unwrap_or("not mounted")),
        ];
        out.extend(self.kernels.iter().map(|k| {
            let image = format!("legionctl-{k}.efi");
            format!(
                "kernel {k}: {}",
                if self.ukis.contains(&image) {
                    image
                } else {
                    "no legionctl image".into()
                }
            )
        }));
        out.join("\n")
    }
}

impl Render for GreeterState {
    fn text(&self) -> String {
        let mut out = vec![
            format!(
                "login screen: {}",
                if self.installed {
                    "legionctl's greeter (greetd)"
                } else {
                    "not legionctl's greeter (legionctl greeter install)"
                }
            ),
            format!(
                "display manager at the next start: {}",
                self.display_manager.as_deref().unwrap_or("none")
            ),
        ];
        if self.kept {
            out.push("the greetd configuration before legionctl's is kept".into());
        }
        out.extend(self.missing.iter().map(|m| format!("not installed: {m}")));
        out.join("\n")
    }
}

impl Render for WindowsVmBegun {
    fn text(&self) -> String {
        self.lines.join("\n")
    }
}

impl Render for VfioDevices {
    fn text(&self) -> String {
        if self.items.is_empty() {
            return "no discrete GPU or NVMe controller with an IOMMU group (is the IOMMU on?)"
                .into();
        }
        let mut out = Vec::new();
        for d in &self.items {
            out.push(format!(
                "{} {} {} group {} ({}) driver {}: {}",
                d.kind,
                d.device,
                d.ids,
                d.group,
                d.members.join(" "),
                d.driver.as_deref().unwrap_or("none"),
                if d.users.is_empty() { "free" } else { "in use" }
            ));
            out.extend(d.users.iter().map(|u| format!("  - {u}")));
        }
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

impl Render for crate::FanState {
    fn text(&self) -> String {
        let levels = |l: &[u8]| {
            l.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mode = self.mode.as_deref().unwrap_or("unknown");
        let mut out = vec![
            if mode == "custom" {
                "mode custom: the fan curve applies".to_string()
            } else {
                format!("mode {mode}: the fan curve applies in custom mode only")
            },
            format!("fan max mode: {}", if self.max { "on" } else { "off" }),
        ];
        match (&self.curve, &self.curve_error) {
            (Some(curve), _) => {
                out.push("fan table now (point: level, fan 1 / fan 2 rpm):".into());
                for (i, level) in curve.iter().enumerate() {
                    let rpm = |fan: &[u32]| {
                        fan.get(usize::from(*level).wrapping_sub(1))
                            .map_or("?".into(), ToString::to_string)
                    };
                    out.push(format!(
                        "  {:>2}: {level:>2}  {} / {} rpm",
                        i + 1,
                        rpm(&self.fan1_rpm),
                        rpm(&self.fan2_rpm)
                    ));
                }
            }
            (None, error) => out.push(format!(
                "fan table now: unknown ({})",
                error.as_deref().unwrap_or("not read")
            )),
        }
        out.push(match &self.chosen {
            Some(c) => format!("your curve: {}", levels(c)),
            None => format!(
                "your curve: none; the default {} applies",
                levels(&self.default_curve)
            ),
        });
        out.push(format!(
            "EC fan lock: {}",
            if self.ec_fan_lock_verified {
                "verified in the profile"
            } else {
                "not verified, refused"
            }
        ));
        out.join("\n")
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
        if self.branch.is_empty() {
            return format!(
                "{}: {} through a layer at {} (sessions see the changes; merge applies them)",
                self.dossier,
                self.repo.display(),
                self.path.display()
            );
        }
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
            if s.workspace.branch.is_empty() {
                out.push(format!(
                    "  {} changes over {}",
                    s.files.len(),
                    s.workspace.repo.display()
                ));
                out.extend(s.files.iter().map(|f| format!("  {f}")));
            } else {
                out.push(format!(
                    "  {} ahead, {} behind; staged {}, unstaged {}, untracked {}, conflicts {}",
                    s.ahead, s.behind, s.staged, s.unstaged, s.untracked, s.conflicted
                ));
            }
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
        let todo = self.items.iter().filter(|i| i.status != "current").count();
        out.push(if todo == 0 {
            "all set up".into()
        } else {
            format!("{todo} items need attention; run `legionctl setup apply`")
        });
        out.join("\n")
    }

    fn success(&self) -> bool {
        self.items.iter().all(|i| i.status == "current")
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
                    l.updated_at,
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
                let runq = s.runq_latency.as_ref().map_or("-".into(), |classes| {
                    classes
                        .iter()
                        .map(|(class, l)| format!("{} {}", &class[..1], l.p99_us))
                        .collect::<Vec<_>>()
                        .join(" ")
                });
                format!(
                    "pkg {}  cpu {} {}  gpu {gpu}  fans {fans} rpm  throttle {throttle}  runq p99 {runq} µs  psi cpu/mem/io {}/{}/{}",
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
                    e.id, e.at, e.actor, e.kind, e.payload
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn knob_line(k: &Knob) -> String {
    let value = k.value.as_deref().unwrap_or("?");
    let desired = match &k.desired {
        Some(d) if Some(d) == k.value.as_ref() => String::new(),
        Some(d) => format!("  (wants {d})"),
        None => String::new(),
    };
    let range = match (&k.min, &k.max) {
        // the firmware gives 0..0 for a value it has no range for
        (Some(min), Some(max)) if min != max => format!("  [{min}..{max}]"),
        _ if !k.choices.is_empty() => format!("  [{}]", k.choices.join(" ")),
        _ => String::new(),
    };
    format!(
        "{:<40} {:<8} {value}{desired}{range}",
        k.id,
        format!("{:?}", k.policy).to_lowercase()
    )
}

impl Render for Vec<Knob> {
    fn text(&self) -> String {
        self.iter().map(knob_line).collect::<Vec<_>>().join("\n")
    }
}

/// Why a knob has its wanted value: each layer that sets it, the winner, and the hold on top.
fn because(k: &Knob) -> String {
    let mut lines = vec!["because:".to_string()];
    let last = k.chain.len().saturating_sub(1);
    for (i, link) in k.chain.iter().enumerate() {
        let value = match &link.value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let wins = if i == last && k.held.is_none() {
            "  <- wins"
        } else {
            ""
        };
        lines.push(format!("  {} ({}): {value}{wins}", link.layer, link.when));
    }
    if k.chain.is_empty() {
        lines.push("  no layer sets it".into());
    }
    lines.push(match &k.held {
        Some(v) => format!("  your hold: {v}  <- wins"),
        None => "  hold: none".into(),
    });
    lines.join("\n")
}

impl Render for KnobDetail {
    fn text(&self) -> String {
        let history = if self.history.items.is_empty() {
            "no events name this knob".into()
        } else {
            self.history.text()
        };
        format!(
            "{}\n{}\n{}\n\n{history}",
            knob_line(&self.knob),
            self.knob.path,
            because(&self.knob)
        )
    }
}

impl Render for Conditions {
    fn text(&self) -> String {
        format!(
            "power {}, Fn+Q {}, workload {}",
            self.power,
            self.fnq.as_deref().unwrap_or("none"),
            if self.workload.is_empty() {
                "none".into()
            } else {
                self.workload.join(" ")
            }
        )
    }
}

fn rule_line(r: &RuleState) -> String {
    let when = if r.layer.when.is_empty() {
        "always".into()
    } else {
        r.layer
            .when
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let state = match (&r.layer.off, r.active) {
        (Some(why), _) => format!("off: {why}"),
        (None, true) => "applies now".into(),
        (None, false) => "on, waits for its conditions".into(),
    };
    format!(
        "{:<28} {:<28} {} knobs  {state}",
        r.id,
        when,
        r.layer.set.len()
    )
}

impl Render for Rules {
    fn text(&self) -> String {
        let mut out = vec![self.conditions.text()];
        out.extend(self.layers.iter().map(rule_line));
        out.join("\n")
    }
}

impl Render for RuleDetail {
    fn text(&self) -> String {
        format!(
            "{}\n{}\n\n{}",
            rule_line(&self.layer),
            self.layer.layer.file,
            self.text.trim_end()
        )
    }
}

impl Render for PlanView {
    fn text(&self) -> String {
        let mut out = vec![self.conditions.text(), String::new()];
        for knob in &self.knobs {
            out.push(knob_line(knob));
            out.push(because(knob));
        }
        out.push(String::new());
        if self.changes.is_empty() {
            out.push("no writes: the machine is in the desired state".into());
        } else {
            out.extend(self.changes.iter().cloned());
        }
        out.join("\n")
    }
}

impl Render for Recording {
    fn text(&self) -> String {
        let section = |title: &str, lines: &[String]| {
            if lines.is_empty() {
                format!("{title}: none")
            } else {
                format!(
                    "{title}:\n{}",
                    lines
                        .iter()
                        .map(|l| format!("  {l}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            }
        };
        let mut out = vec![if self.paths > 0 {
            format!("recorded {} machine paths into {}", self.paths, self.dir)
        } else {
            format!("replay of {}", self.dir)
        }];
        out.push(section("power apply would", &self.plan));
        out.push(section("doctor", &self.findings));
        out.push(format!("knobs:\n{}", self.knobs.text()));
        out.push(format!(
            "events ({}):\n{}",
            self.events.len(),
            self.events.text()
        ));
        out.join("\n\n")
    }
}

/// The numbers of a run that `bench compare` lines up: scores first, then the telemetry summary.
fn bench_rows(run: &BenchRun) -> Vec<(String, Option<f64>)> {
    let s = &run.result.summary;
    let mut rows: Vec<(String, Option<f64>)> = run
        .result
        .scores
        .iter()
        .map(|(k, v)| (k.clone(), Some(*v)))
        .collect();
    rows.extend([
        ("package W mean".into(), s.package_w_mean),
        ("package W max".into(), s.package_w_max),
        ("CPU °C max".into(), s.cpu_temp_max),
        ("CPU MHz mean".into(), s.cpu_mhz_mean),
        ("GPU W mean".into(), s.gpu_w_mean),
        ("GPU °C max".into(), s.gpu_temp_max),
        (
            "fan rpm max".into(),
            s.fan_rpm_max
                .map(|r| f64::from(u32::try_from(r).unwrap_or(u32::MAX))),
        ),
        (
            "runq p99 µs max".into(),
            s.runq_p99_us_max
                .map(|r| f64::from(u32::try_from(r).unwrap_or(u32::MAX))),
        ),
    ]);
    rows
}

fn bench_title(run: &BenchRun) -> String {
    let r = &run.result;
    let scenario = serde_json::to_value(r.scenario)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    format!(
        "run {} {scenario} {} s{}",
        run.id,
        r.seconds,
        r.label
            .as_ref()
            .map(|l| format!(" '{l}'"))
            .unwrap_or_default()
    )
}

impl Render for BenchRun {
    fn text(&self) -> String {
        let mut out = vec![bench_title(self)];
        out.extend(
            bench_rows(self)
                .into_iter()
                .map(|(k, v)| format!("  {k:<20} {}", some(v, ""))),
        );
        let throttle = &self.result.summary.throttle;
        out.push(format!(
            "  {:<20} {}",
            "throttle",
            if throttle.is_empty() {
                "-".into()
            } else {
                throttle.join(", ")
            }
        ));
        out.extend(self.result.problems.iter().map(|p| format!("problem: {p}")));
        out.join("\n")
    }

    /// GPU computation errors or a failed stressor fail the command.
    fn success(&self) -> bool {
        self.result.problems.is_empty()
    }
}

impl Render for Vec<BenchRun> {
    fn text(&self) -> String {
        self.iter()
            .map(|r| {
                let scores: Vec<String> = r
                    .result
                    .scores
                    .iter()
                    .map(|(k, v)| format!("{k} {v:.0}"))
                    .collect();
                format!("{:<36} {}", bench_title(r), scores.join(", "))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for BenchCompare {
    fn text(&self) -> String {
        let (a, b) = (bench_rows(&self.a), bench_rows(&self.b));
        let mut out = vec![format!(
            "a: {}\nb: {}\n",
            bench_title(&self.a),
            bench_title(&self.b)
        )];
        out.push(format!(
            "{:<20} {:>10} {:>10} {:>8}",
            "", "a", "b", "change"
        ));
        for (name, va) in &a {
            let vb = b.iter().find(|(n, _)| n == name).and_then(|(_, v)| *v);
            let change = va
                .zip(vb)
                .filter(|(x, _)| *x != 0.0)
                .map_or("-".into(), |(x, y)| format!("{:+.1}%", (y - x) / x * 100.0));
            out.push(format!(
                "{name:<20} {:>10} {:>10} {change:>8}",
                some(*va, ""),
                some(vb, "")
            ));
        }
        let knobs: Vec<String> = self
            .a
            .result
            .knobs
            .iter()
            .filter_map(|(id, va)| {
                let vb = self.b.result.knobs.get(id).cloned().flatten();
                (*va != vb).then(|| {
                    format!(
                        "{id}: {} -> {}",
                        va.as_deref().unwrap_or("-"),
                        vb.as_deref().unwrap_or("-")
                    )
                })
            })
            .collect();
        out.push(String::new());
        out.push(if knobs.is_empty() {
            "knobs: the same".into()
        } else {
            format!("knobs that differ:\n  {}", knobs.join("\n  "))
        });
        out.join("\n")
    }
}

impl Render for TuneReport {
    fn text(&self) -> String {
        let mut out = vec![format!(
            "{:<8} {:>16} {:>14} {:>10} {:>8}  steady throttle",
            self.attribute.rsplit('_').next().unwrap_or_default(),
            "score",
            "package W",
            "CPU °C",
            "GPU W"
        )];
        for step in &self.steps {
            let score = step.scores.values().next().copied();
            out.push(format!(
                "{:<8} {:>16} {:>14} {:>10} {:>8}  {}",
                step.value,
                some(score, ""),
                some(step.summary.package_w_mean, ""),
                some(step.summary.cpu_temp_max, ""),
                some(step.summary.gpu_w_mean, ""),
                if step.steady_throttle.is_empty() {
                    "-".into()
                } else {
                    step.steady_throttle.join(",")
                },
            ));
            out.extend(
                step.problems
                    .iter()
                    .map(|p| format!("         problem: {p}")),
            );
        }
        out.push(String::new());
        out.push(match self.recommended {
            Some(v) => format!("recommended {} = {v}: {}", self.attribute, self.reason),
            None => format!("no recommendation: {}", self.reason),
        });
        if let Some(why) = &self.aborted {
            out.push(format!("stopped early: {why}"));
        }
        out.push(format!(
            "{} is back at {}; results in {}",
            self.attribute, self.original, self.file
        ));
        out.join("\n")
    }

    /// A sweep that stopped early fails the command.
    fn success(&self) -> bool {
        self.aborted.is_none()
    }
}

impl Render for WindowsState {
    fn text(&self) -> String {
        let mut out = vec![match (&self.boot_entry, &self.boot_entry_label) {
            (Some(id), Some(label)) => format!("Windows boots from firmware entry {id} ({label})"),
            _ => "no firmware entry starts Windows".into(),
        }];
        if let Some(next) = &self.boot_next {
            out.push(format!("one-time boot set: {next}"));
        }
        out.push(match &self.system_hive {
            Some(hive) => format!("registry: {hive}"),
            None => {
                "registry: not checked (mount the Windows volume read-only with ntfs3 to check it)"
                    .into()
            }
        });
        out.extend(self.hazards.iter().map(|h| format!("- {h}")));
        out.join("\n")
    }

    /// Hazards fail the command.
    fn success(&self) -> bool {
        self.hazards.is_empty()
    }
}

impl Render for Vec<ToolCallRow> {
    fn text(&self) -> String {
        self.iter()
            .map(|r| {
                let c = &r.call;
                format!(
                    "{:>6} {} {:<10} {}.{:<24} {:<8} {:>6} ms {:>8} B  {}",
                    r.id,
                    r.at,
                    &c.session.0[..c.session.0.len().min(8)],
                    c.server,
                    c.tool,
                    wire_name(&c.outcome),
                    c.ms,
                    c.bytes,
                    c.args_hash
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for Empty {
    fn text(&self) -> String {
        String::new()
    }
}

impl Render for Vec<PackageState> {
    fn text(&self) -> String {
        self.iter()
            .map(|p| {
                let state = match (&p.manifest, p.intact) {
                    (None, _) => "not fetched".to_string(),
                    (Some(m), Some(true)) => format!("ok {}", &m.hash[..16]),
                    (Some(_), _) => "CHANGED since fetch".to_string(),
                };
                format!(
                    "{:<20} {:<6} {}@{}  {state}",
                    p.name,
                    format!("{:?}", p.kind).to_lowercase(),
                    p.source,
                    p.version
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A changed package fails the command.
    fn success(&self) -> bool {
        self.iter().all(|p| p.intact != Some(false))
    }
}

impl Render for Manifest {
    fn text(&self) -> String {
        format!("{} {} pinned: {}", self.name, self.version, self.hash)
    }
}

impl Render for crate::CacheValue {
    fn text(&self) -> String {
        self.value.clone().unwrap_or_default()
    }
}

impl Render for crate::RunOutput {
    fn text(&self) -> String {
        let mut out = vec![self.text.clone()];
        out.extend(self.permissions.iter().map(|p| format!("permission: {p}")));
        out.push(format!("session {} in {} ms", self.id, self.ms));
        out.join("\n")
    }
}

impl Render for crate::Graph {
    fn text(&self) -> String {
        let mut out: Vec<String> = self
            .nodes
            .iter()
            .map(|n| {
                let status = n
                    .status
                    .as_deref()
                    .map(|s| format!(" [{s}]"))
                    .unwrap_or_default();
                format!("{} ({}){status}: {}", n.id, n.kind, n.title)
            })
            .collect();
        out.extend(
            self.edges
                .iter()
                .map(|e| format!("  {} -{}-> {}", e.from, e.kind, e.to)),
        );
        out.extend(self.history.iter().map(|h| {
            format!(
                "  {} {}: {} -> {}",
                h.at,
                h.session,
                h.from.as_deref().unwrap_or("-"),
                h.to.as_deref().unwrap_or("-")
            )
        }));
        out.join("\n")
    }
}

impl Render for Vec<crate::GatewayUsage> {
    fn text(&self) -> String {
        self.iter()
            .map(|u| {
                format!(
                    "{:>6} {} {:<14} {:<24} in {:>7} out {:>7} ${:.4} {} {} {} ms",
                    u.id,
                    u.at,
                    u.account,
                    u.model,
                    u.tokens_in,
                    u.tokens_out,
                    u.cost_usd,
                    if u.cached { "cached" } else { "called" },
                    u.status,
                    u.ms
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for crate::Edit {
    fn text(&self) -> String {
        let mut out = vec![format!(
            "{}{}",
            self.path.display(),
            if self.dry_run {
                " (dry run: nothing written)"
            } else {
                ""
            }
        )];
        if self.changes.is_empty() {
            out.push("no change".into());
        }
        out.extend(self.changes.iter().cloned());
        out.extend(self.warnings.iter().map(|w| format!("warning: {w}")));
        if let Some(backup) = &self.backup {
            out.push(format!("backup: {}", backup.display()));
        }
        out.join("\n")
    }
}

impl Render for crate::UsageReport {
    fn text(&self) -> String {
        let mut out: Vec<String> = self
            .rows
            .iter()
            .map(|u| {
                format!(
                    "{} {:<7} {:<28} in {:>10} out {:>9} cache read {:>11} write {:>10}",
                    u.day, u.agent, u.model, u.input, u.output, u.cache_read, u.cache_write
                )
            })
            .collect();
        out.extend(
            self.codex_limits
                .iter()
                .map(|(w, p)| format!("codex limit {w}: {p:.0}% used")),
        );
        out.join("\n")
    }
}

impl Render for crate::VirtStatus {
    fn text(&self) -> String {
        let mut out: Vec<String> = self
            .capabilities
            .iter()
            .map(|c| {
                format!(
                    "{} {:<26} {:<16} {}",
                    if c.up { "up  " } else { "DOWN" },
                    c.name,
                    c.provider,
                    c.detail
                )
            })
            .collect();
        out.extend(self.instances.iter().map(|i| {
            let consumer = i
                .consumer
                .as_deref()
                .map(|c| format!(" ({c})"))
                .unwrap_or_default();
            let memory = i
                .memory_bytes
                .map(|b| format!(", holds {}", legionctl_machine::human_bytes(b)))
                .unwrap_or_default();
            format!(
                "  {:<9} {}{consumer}: {} {}{memory}",
                wire_name(&i.kind),
                i.name,
                i.state,
                i.image
            )
        }));
        out.extend(self.findings.iter().map(|f| format!("- {f}")));
        out.join("\n")
    }

    /// Findings fail the command.
    fn success(&self) -> bool {
        self.findings.is_empty()
    }
}

impl Render for crate::Scan {
    fn text(&self) -> String {
        if self.services.is_empty() {
            return "no Dockerfile or compose file in this repository".into();
        }
        let mut out: Vec<String> = self
            .compose_file
            .iter()
            .map(|f| format!("compose file {f}"))
            .collect();
        for (name, s) in &self.services {
            let source = match (&s.build, &s.image) {
                (Some(b), _) => format!(
                    "{}{}",
                    b.dockerfile,
                    b.target
                        .as_deref()
                        .map(|t| format!(" (stage {t})"))
                        .unwrap_or_default()
                ),
                (None, image) => format!("image {}", image.as_deref().unwrap_or("?")),
            };
            out.push(format!("{name}: {source}, ports {}", s.ports.join(" ")));
            out.extend(s.mounts.iter().map(|m| {
                let mode = if m.read_only { "ro" } else { "rw" };
                format!("  mount {} {} -> {} ({mode})", m.kind, m.source, m.target)
            }));
            if !s.depends_on.is_empty() {
                out.push(format!("  depends on {}", s.depends_on.join(", ")));
            }
            out.extend(
                s.entrypoints
                    .iter()
                    .map(|(k, v)| format!("  {k}: {}", v.join(" "))),
            );
            if let Some(c) = self.cache.get(name) {
                out.push(format!(
                    "  bases: {}; lockfiles: {}",
                    c.bases.join(", "),
                    c.lockfiles.join(", ")
                ));
            }
        }
        out.join("\n")
    }
}

impl Render for Vec<crate::ServiceState> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no containers of this repository".into();
        }
        self.iter()
            .map(|s| {
                let health = s
                    .health
                    .as_deref()
                    .map(|h| format!(" ({h})"))
                    .unwrap_or_default();
                format!("{:<16} {:<28} {}{health}", s.service, s.container, s.state)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// An unhealthy or stopped service fails the command.
    fn success(&self) -> bool {
        self.iter()
            .all(|s| s.state == "running" && s.health.as_deref() != Some("unhealthy"))
    }
}

impl Render for Vec<crate::SessionExec> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no programs recorded for this session".into();
        }
        self.iter()
            .map(|e| format!("{} {:>7} {}", e.at, e.pid, e.argv.join(" ")))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for Vec<crate::Plugin> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no Claude Code plugins".into();
        }
        self.iter()
            .map(|p| {
                format!(
                    "{} {} ({}, {}){}",
                    p.id,
                    p.version,
                    p.scope,
                    if p.enabled { "enabled" } else { "disabled" },
                    if p.allowed {
                        ""
                    } else {
                        ": outside plugins.marketplaces"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for Vec<crate::Account> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no account profiles (legionctl agents accounts add <name>)".into();
        }
        self.iter()
            .map(|a| {
                let yes = |b: bool| if b { "logged in" } else { "not logged in" };
                format!(
                    "{:<16} claude {:<14} codex {:<14} {}",
                    a.name,
                    yes(a.claude),
                    yes(a.codex),
                    a.dir
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// An enum value as the wire names it (`cached`, `vm`).
fn wire_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// A page as its items, and how to get the next one.
impl<T> Render for crate::Page<T>
where
    Vec<T>: Render,
{
    fn text(&self) -> String {
        let items = self.items.text();
        match &self.next {
            Some(next) => format!("{items}\n(more: add --cursor {next})"),
            None => items,
        }
    }
}

impl Render for Vec<crate::CacheStat> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "the cache is empty".into();
        }
        self.iter()
            .map(|c| {
                format!(
                    "{:<8} {:>7} entries  {} of {}",
                    c.ns,
                    c.entries,
                    legionctl_machine::human_bytes(c.bytes),
                    legionctl_machine::human_bytes(c.max_bytes)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for Vec<crate::Job> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no jobs".into();
        }
        self.iter()
            .map(|j| {
                format!(
                    "{:>5} {:<13} {:<8} attempt {}",
                    j.id, j.kind, j.state, j.attempts
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for crate::Job {
    fn text(&self) -> String {
        let end = match (&self.result, &self.error) {
            (Some(result), _) => result.to_string(),
            (None, Some(error)) => error.clone(),
            (None, None) => "running".into(),
        };
        format!(
            "job {} {} {} (attempt {})\ninput: {}\n{end}",
            self.id, self.kind, self.state, self.attempts, self.input
        )
    }
}
