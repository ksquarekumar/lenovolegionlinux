//! Terminal text for each output type. JSON surfaces (MCP, `--json`) do not use it.

use std::collections::BTreeMap;

use crate::{
    BatteryState, BenchCompare, BenchRun, BootState, CapabilityList, Changes, Conditions,
    ConfigFile, ConfigView, DossierFindings, DossierInfo, Empty, EventRow, Explanation, Findings,
    FirmwareDevices, GpuState, GreeterState, Guard, HookReply, HookTable, KernelBench, Knob,
    KnobDetail, Launch, LayerList, Lines, LoadState, Lock, Manifest, PackageState, PlanView,
    Reading, Recording, Reminder, RgbState, RuleDetail, RuleState, Rules, RunEnv, RunnableGrant,
    RunnablesCheck, Sample, SessionState, SetupReport, SkillInfo, Snapshot, TestDetail, TestReview,
    TestRow, ToolCallRow, TuneReport, Validation, VfioDevices, WindowsState, WindowsVmBegun,
    Workspace, WorkspaceStatus,
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
                .map_or("unknown (legionctl_wmi is not loaded)".into(), |f| {
                    f.values()
                        .map(|r| format!("{r} rpm"))
                        .collect::<Vec<_>>()
                        .join(" / ")
                }),
            s.throttle_reasons.as_ref().map_or_else(
                || format!(
                    "unknown ({})",
                    s.throttle_unknown.as_deref().unwrap_or_default()
                ),
                |r| if r.is_empty() {
                    "none".into()
                } else {
                    r.join(", ")
                }
            )
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
            let lines: String = self
                .findings
                .iter()
                .map(|f| {
                    // shortcut: a string is a positional word and a true bool a flag, as for the fixes of now; an
                    // action with a named text option needs the CLI's own words from the operation's schema
                    let fixes: Vec<String> =
                        f.actions
                            .iter()
                            .map(|a| {
                                let words = a.input.as_object().into_iter().flatten().filter_map(
                                    |(k, v)| match v {
                                        serde_json::Value::String(s) => Some(s.clone()),
                                        serde_json::Value::Bool(true) => {
                                            Some(format!("--{}", k.replace('_', "-")))
                                        }
                                        _ => None,
                                    },
                                );
                                std::iter::once(format!("legionctl {}", a.op.replace('.', " ")))
                                    .chain(words)
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .collect();
                    if fixes.is_empty() {
                        format!("\n- {}", f.text)
                    } else {
                        format!("\n- {} (fix: {})", f.text, fixes.join("; "))
                    }
                })
                .collect();
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
        if let Some(p) = &self.nvidia_power {
            let or = |v: &Option<String>| v.clone().unwrap_or_else(|| "unknown".into());
            out.push(format!(
                "NVIDIA GPU power: {} (power/control {})",
                or(&p.runtime_status),
                or(&p.control)
            ));
            out.extend(p.users.iter().map(|u| format!("  keeps it awake: {u}")));
            out.extend(not_visible(p.not_visible).map(|n| format!("  {n}")));
        }
        out.join("\n")
    }
}

/// The line for processes whose files a report as the user may not read; none when it saw all.
fn not_visible(n: usize) -> Option<String> {
    (n > 0).then(|| {
        format!("{n} processes not visible (another user's, or not dumpable): their open files are not checked")
    })
}

impl Render for FirmwareDevices {
    fn text(&self) -> String {
        if let Some(missing) = &self.missing {
            return missing.clone();
        }
        if self.devices.is_empty() {
            return "fwupd knows no devices".into();
        }
        self.devices
            .iter()
            .map(|d| {
                let version = d.version.as_deref().unwrap_or("unknown");
                let state = match (&d.update, d.updatable) {
                    (Some(u), _) => format!(
                        "update to {u}{}",
                        d.update_summary
                            .as_ref()
                            .map_or_else(String::new, |s| format!(": {s}"))
                    ),
                    (None, true) => "no update".into(),
                    (None, false) => "fwupd cannot update it".into(),
                };
                format!("{} {version} - {state}\n  id {}", d.name, d.id)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for RgbState {
    fn text(&self) -> String {
        let mut out = vec![format!(
            "keyboard (ITE 8258, USB 048d:c988): {}",
            if self.keyboard_usb {
                "on the USB bus"
            } else {
                "not found"
            }
        )];
        match self.protocol {
            Some(p) => out.push(format!("OpenRGB at {}: SDK protocol {p}", self.address)),
            None => out.push(format!("OpenRGB at {}: no answer", self.address)),
        }
        out.extend(self.controllers.iter().map(|c| format!("  device: {c}")));
        out.extend(self.profiles.iter().map(|p| format!("  profile: {p}")));
        if let Some(problem) = &self.problem {
            out.push(format!("problem: {problem}"));
        }
        out.join("\n")
    }

    fn success(&self) -> bool {
        self.problem.is_none()
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
                Some(id) if self.order.first() == Some(id) => {
                    format!("legionctl's entry: {}, the default", label(id))
                }
                Some(id) if self.next.as_ref() == Some(id) => format!(
                    "legionctl's entry: {}. {}",
                    label(id),
                    legionctl_machine::boot::next_start(
                        &self
                            .current
                            .as_deref()
                            .map_or("the entry that started this computer".into(), label)
                    )
                ),
                Some(id) if self.current.as_ref() == Some(id) => format!(
                    "legionctl's entry: {}. This start used it: it becomes the default when the login screen is up.",
                    label(id)
                ),
                Some(id) => format!(
                    "legionctl's entry: {}, not the default: the last start did not use it. To try it again: \
                     legionctl boot install, then restart.",
                    label(id)
                ),
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

impl Render for crate::Kernels {
    fn text(&self) -> String {
        let mut out = Vec::new();
        if !self.takeover {
            out.push(
                "boot takeover: not installed (legionctl boot install); kernel default and kernel try need it"
                    .to_string(),
            );
        }
        for k in &self.kernels {
            let marks: Vec<&str> = [(k.running, "running"), (k.default, "default")]
                .into_iter()
                .filter_map(|(on, mark)| on.then_some(mark))
                .collect();
            out.push(format!(
                "{} {} [{}]{}",
                k.package,
                k.release,
                if k.flavour.is_empty() {
                    "flavour unknown: no headers".into()
                } else {
                    k.flavour.join(" ")
                },
                if marks.is_empty() {
                    String::new()
                } else {
                    format!(" {}", marks.join(", "))
                }
            ));
            out.push(format!("  image: {}", k.image.as_deref().unwrap_or("none")));
            out.push(format!(
                "  command line: {}",
                k.cmdline.as_deref().unwrap_or("none yet")
            ));
            out.push(format!("  dkms: {}", k.dkms.join(", ")));
        }
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
            out.extend(not_visible(d.not_visible).map(|n| format!("  {n}")));
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
                "available"
            } else {
                "not available on this machine"
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

impl Render for WorkspaceStatus {
    fn text(&self) -> String {
        let mut out = vec![self.workspace.text()];
        if self.workspace.branch.is_empty() {
            out.push(format!(
                "  {} changes over {}",
                self.files.len(),
                self.workspace.repo.display()
            ));
            out.extend(self.files.iter().map(|f| format!("  {f}")));
        } else {
            out.push(format!(
                "  {} ahead, {} behind; staged {}, unstaged {}, untracked {}, conflicts {}",
                self.ahead,
                self.behind,
                self.staged,
                self.unstaged,
                self.untracked,
                self.conflicted
            ));
        }
        out.extend(self.drift.iter().map(|d| format!("  drift: {d}")));
        out.join("\n")
    }

    fn success(&self) -> bool {
        self.drift.is_empty()
    }
}

impl Render for RunnablesCheck {
    fn text(&self) -> String {
        let mut out = vec![format!(
            "{} for {}",
            self.team
                .as_deref()
                .map_or("each agent config".into(), |t| format!("team {t}")),
            self.harness
        )];
        for m in &self.members {
            out.push(format!(
                "{} ({}, config {}) on {}",
                m.name, m.archetype, m.config, m.run_path
            ));
            for (field, value) in &m.fields {
                let by = m
                    .set_by
                    .get(field)
                    .map_or(String::new(), |b| format!("  [{b}]"));
                out.push(format!("  {field}: {value}{by}"));
            }
        }
        let usable = self.sets.iter().filter(|s| s.refusal.is_none()).count();
        out.push(format!(
            "capability sets: {} ({usable} usable)",
            self.sets.len()
        ));
        for s in &self.sets {
            out.push(format!(
                "  {}@{} [{}] {}{}",
                s.name,
                s.version,
                s.layer,
                s.trust,
                s.refusal
                    .as_deref()
                    .map_or(String::new(), |r| format!(": {r}"))
            ));
        }
        out.extend(self.refused.iter().map(|r| format!("refused: {r}")));
        out.push(if self.refused.is_empty() {
            "ok".into()
        } else {
            format!("{} refused", self.refused.len())
        });
        out.join("\n")
    }

    fn success(&self) -> bool {
        self.refused.is_empty()
    }
}

impl Render for RunnableGrant {
    /// Counts first, one line per part; the manifest's detail is in `--json`.
    fn text(&self) -> String {
        let m = &self.manifest;
        let count = |part: &str, key: &str| m[part][key].as_array().map_or(0, Vec::len);
        let sets: Vec<String> = self
            .sets
            .iter()
            .map(|s| format!("{}@{} [{}]", s.name, s.version, s.layer))
            .collect();
        let hosts: Vec<&str> = m["connections"]["out"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e["host"].as_str())
            .collect();
        let storage: Vec<String> = m["storage"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                format!(
                    "{} ({})",
                    s["path"].as_str().unwrap_or("?"),
                    s["mode"].as_str().unwrap_or("?")
                )
            })
            .collect();
        let mut out = vec![
            format!(
                "{}: {} sets: {}",
                self.runnable,
                sets.len(),
                sets.join(", ")
            ),
            format!(
                "connections: {} out ({}), {} in, {} forbidden, web read {}",
                hosts.len(),
                hosts.join(", "),
                count("connections", "in"),
                count("connections", "forbidden"),
                if m["connections"]["web_read"] == true {
                    "on"
                } else {
                    "off"
                }
            ),
            format!(
                "storage: {}",
                if storage.is_empty() {
                    "none".into()
                } else {
                    storage.join(", ")
                }
            ),
            format!(
                "host: {} privileges, {} devices, {} forbidden commands",
                count("host", "privileges"),
                count("host", "devices"),
                count("host", "forbidden_commands")
            ),
        ];
        if let Some(agent) = m["harness"]["agent"].as_str() {
            out.push(format!(
                "harness: {agent} {}",
                m["harness"]["versions"].as_str().unwrap_or_default()
            ));
        }
        if let Some(parent) = &self.parent {
            out.push(format!("parent: {}", parent.join(", ")));
        }
        out.join("\n")
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
                let id = d.id.as_deref().unwrap_or("(no id)");
                let chosen = if d.chosen {
                    "  (the user's dossier)"
                } else {
                    ""
                };
                if d.has_plan {
                    format!("{}  {id}{chosen}", d.name)
                } else {
                    format!("{}  {id}  (no plan.md){chosen}", d.name)
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

impl Render for Vec<Reading> {
    fn text(&self) -> String {
        let mut out = Vec::new();
        let mut source = "";
        for r in self {
            if r.source != source {
                source = &r.source;
                out.push(format!("{source}:"));
            }
            let value = r.value.map_or_else(
                || r.text.clone().unwrap_or_default(),
                |v| format!("{v:.1} {}", r.unit),
            );
            let range = r
                .history
                .iter()
                .copied()
                .reduce(f64::min)
                .zip(r.history.iter().copied().reduce(f64::max));
            match range {
                Some((low, high)) if r.history.len() > 1 => out.push(format!(
                    "  {:<34} {value:<14} ({low:.1}-{high:.1} in {} samples)",
                    r.name,
                    r.history.len()
                )),
                _ => out.push(format!("  {:<34} {value}", r.name)),
            }
        }
        out.join("\n")
    }
}

impl Render for Vec<Sample> {
    fn text(&self) -> String {
        self.iter()
            .map(|s| {
                let gpu = s.gpu.as_ref().map_or("-".into(), |g| {
                    g.error.as_ref().map_or_else(
                        || format!("{} {} {}", g.state, some(g.power_w, " W"), some(g.temp_c, " °C")),
                        |e| format!("{} ({e})", g.state),
                    )
                });
                let fans = s.root.fan_rpm.as_ref().map_or("?".into(), |f| {
                    f.values().map(ToString::to_string).collect::<Vec<_>>().join("/")
                });
                let throttle = s.root.throttle_now.as_ref().map_or("?".into(), |t| {
                    if t.is_empty() { "-".into() } else { t.join(",") }
                });
                let runq = s.runq_latency.as_ref().map_or("-".into(), |classes| {
                    classes
                        .iter()
                        .map(|(class, l)| format!("{} {}", &class[..1], l.p99_us))
                        .collect::<Vec<_>>()
                        .join(" ")
                });
                let top = s.cpu_energy.as_ref().and_then(|e| e.first()).map_or("-".into(), |e| {
                    format!("{} {:.0}%", e.name, e.share_percent)
                });
                format!(
                    "pkg {}  cpu {} {}  gpu {gpu}  fans {fans} rpm  throttle {throttle}  runq p99 {runq} µs  top {top}  psi cpu/mem/io {}/{}/{}",
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
            "power {}, Fn+Q {}, workload {}, kernel {}",
            self.power,
            self.fnq.as_deref().unwrap_or("none"),
            if self.workload.is_empty() {
                "none".into()
            } else {
                self.workload.join(" ")
            },
            self.kernel.as_deref().unwrap_or("unknown")
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
        "run {} {scenario} {} s{}{}",
        run.id,
        r.seconds,
        r.label
            .as_ref()
            .map(|l| format!(" '{l}'"))
            .unwrap_or_default(),
        r.kernel
            .as_ref()
            .map(|k| format!(" on {k} ({})", r.release.as_deref().unwrap_or("-")))
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

impl Render for KernelBench {
    /// One column per kernel; the change of each against the first kernel.
    fn text(&self) -> String {
        let mut out = Vec::new();
        if let Some(first) = self.runs.first() {
            let kernel = |r: &BenchRun| r.result.kernel.clone().unwrap_or_default();
            out.extend(self.runs.iter().map(bench_title));
            out.push(String::new());
            let header: Vec<String> = self
                .runs
                .iter()
                .map(|r| format!("{:>24}", kernel(r)))
                .collect();
            out.push(format!("{:<20} {}", "", header.join("")));
            let rows: Vec<Vec<(String, Option<f64>)>> = self.runs.iter().map(bench_rows).collect();
            for (name, base) in bench_rows(first) {
                let cells: Vec<String> = rows
                    .iter()
                    .enumerate()
                    .map(|(i, row)| {
                        let v = row.iter().find(|(n, _)| *n == name).and_then(|(_, v)| *v);
                        let change = base
                            .zip(v)
                            .filter(|(x, _)| i > 0 && *x != 0.0)
                            .map(|(x, y)| format!(" {:+.1}%", (y - x) / x * 100.0))
                            .unwrap_or_default();
                        format!("{:>24}", format!("{}{change}", some(v, "")))
                    })
                    .collect();
                out.push(format!("{name:<20} {}", cells.join("")));
            }
        } else if self.lines.is_empty() {
            out.push(
                "no run of this suite on any kernel yet (legionctl kernel bench <suite> --all)"
                    .into(),
            );
        }
        if !self.lines.is_empty() && !out.is_empty() {
            out.push(String::new());
        }
        out.extend(self.lines.iter().cloned());
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
        if let Some(acp) = &self.acp {
            out.push(format!(
                "ACP session {acp}: give --acp {acp} to go on in it"
            ));
        }
        out.push(format!("session {} in {} ms", self.id, self.ms));
        out.join("\n")
    }
}

impl Render for Vec<crate::AgentCommand> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "the agent offered no commands".into();
        }
        self.iter()
            .map(|c| {
                let hint = c
                    .hint
                    .as_deref()
                    .map(|h| format!(" <{h}>"))
                    .unwrap_or_default();
                format!("/{}{hint}  {}", c.name, c.description)
            })
            .collect::<Vec<_>>()
            .join("\n")
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
        self.rows
            .iter()
            .map(|u| {
                format!(
                    "{} {:<7} {:<28} in {:>10} out {:>9} cache read {:>11} write {:>10}",
                    u.day, u.agent, u.model, u.input, u.output, u.cache_read, u.cache_write
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
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
            let memory = match (i.memory_bytes, &i.memory_unknown) {
                (Some(b), _) => format!(", holds {}", legionctl_machine::human_bytes(b)),
                (None, Some(why)) => format!(", memory unknown: {why}"),
                (None, None) => String::new(),
            };
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
            return "no programs or files recorded for this session".into();
        }
        self.iter()
            .map(|e| match &e.path {
                Some(path) => format!("{} {:>7} open {path}", e.at, e.pid),
                None => format!("{} {:>7} {}", e.at, e.pid, e.argv.join(" ")),
            })
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

/// Where an account's login is, in words: `current login`, `clauth (active)`, `legionctl`.
fn from(source: &str, active: bool) -> String {
    match (source, active) {
        ("current", _) => "current login".into(),
        (s, true) => format!("{s} (active)"),
        (s, false) => s.into(),
    }
}

impl Render for Vec<crate::AccountUsage> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no login: Claude Code and Codex are not logged in, and no clauth account has a login"
                .into();
        }
        self.iter()
            .map(|u| format!("{:<16} {}", u.account, u.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for Vec<crate::Account> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no login: Claude Code and Codex are not logged in, and no clauth account has a login"
                .into();
        }
        self.iter()
            .map(|a| {
                let yes = |b: bool| if b { "logged in" } else { "not logged in" };
                format!(
                    "{:<16} {:<20} claude {:<14} codex {}",
                    a.name,
                    from(&a.source, a.active),
                    yes(a.claude),
                    yes(a.codex),
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

impl Render for crate::ResourceList {
    fn text(&self) -> String {
        let mut out = Vec::new();
        let mut group = "";
        for r in &self.resources {
            if r.group != group {
                group = &r.group;
                out.push(format!("{group}:"));
            }
            out.push(format!("  {}  {}  {}", r.id, r.name, r.state));
            if !r.users.is_empty() {
                out.push(format!("    used by: {}", r.users.join(", ")));
            }
            if !r.actions.is_empty() {
                let actions: Vec<String> = r
                    .actions
                    .iter()
                    .map(|a| format!("{} ({})", a.name, a.op))
                    .collect();
                out.push(format!("    actions: {}", actions.join(", ")));
            }
        }
        out.extend(not_visible(self.not_visible));
        out.join("\n")
    }
}

impl Render for crate::Acted {
    fn text(&self) -> String {
        match &self.output {
            // as the routed operation shows it
            Some(output) => crate::render(&self.op, output.clone())
                .map_or_else(|e| format!("{}: {e:#}", self.op), |(text, _)| text),
            None => format!("dry run: would run {} {}", self.op, self.input),
        }
    }

    fn success(&self) -> bool {
        self.output.as_ref().is_none_or(|output| {
            crate::render(&self.op, output.clone()).is_ok_and(|(_, success)| success)
        })
    }
}

impl Render for crate::GraphRefs {
    fn text(&self) -> String {
        let mut out: Vec<String> = self
            .refs
            .iter()
            .map(|r| {
                let short = r.tree.get(..12).unwrap_or(&r.tree);
                let graph = if r.graph { "graph" } else { "no graph" };
                format!("{:<32} tree {short}  {graph}", r.name)
            })
            .collect();
        out.push(format!("{} snapshots:", self.snapshots.len()));
        out.extend(self.snapshots.iter().map(|s| {
            format!(
                "  {}  {} KiB  {}",
                s.tree,
                s.size_bytes / 1024,
                s.refs.join(", ")
            )
        }));
        out.join("\n")
    }
}

impl Render for crate::CodeNodes {
    fn text(&self) -> String {
        if self.nodes.is_empty() {
            return format!("no symbols (tree {})", self.tree);
        }
        self.nodes
            .iter()
            .map(|n| {
                format!(
                    "{:<10} {}  {}:{}",
                    n.kind,
                    n.key,
                    n.file,
                    n.line.unwrap_or(0)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The kinds of entity that `code graph changes` lists by default; the other kinds are a count per file.
const CODE_KINDS: [&str; 7] = [
    "function", "method", "struct", "enum", "impl", "constant", "module",
];

impl Render for Vec<crate::ChangeSet> {
    fn text(&self) -> String {
        let mut out = Vec::new();
        for (at, set) in self.iter().enumerate() {
            out.push(format!(
                "{} -> {} (change set {})",
                set.from, set.to, set.id
            ));
            let mut files: BTreeMap<&str, Vec<&crate::EntityChange>> = BTreeMap::new();
            for e in &set.entities {
                files.entry(&e.file).or_default().push(e);
            }
            for (file, rows) in files {
                // kind -> [added, removed, modified, moved]
                let mut kinds: BTreeMap<&str, [usize; 4]> = BTreeMap::new();
                for e in rows.iter().filter(|e| e.kind != "file") {
                    let at = ["added", "removed", "modified", "moved"]
                        .iter()
                        .position(|c| *c == e.change)
                        .unwrap_or(2);
                    kinds.entry(&e.kind).or_default()[at] += 1;
                }
                let counts: Vec<String> = kinds
                    .iter()
                    .map(|(kind, n)| {
                        let signs = ["+", "-", "~", ">"]
                            .iter()
                            .zip(n)
                            .filter(|(_, n)| **n > 0)
                            .map(|(sign, n)| format!("{sign}{n}"));
                        format!("{kind} {}", signs.collect::<Vec<_>>().join(" "))
                    })
                    .collect();
                let lines = rows
                    .iter()
                    .find(|e| e.kind == "file")
                    .map(|e| format!("{} {}", e.change, e.lines))
                    .unwrap_or_default();
                out.push(format!("  {file}  {lines}  {}", counts.join(", ")));
                out.extend(
                    rows.iter()
                        .filter(|e| {
                            e.kind != "file"
                                && (set.detail || CODE_KINDS.contains(&e.kind.as_str()))
                        })
                        .map(|e| {
                            format!(
                                "    {:<8} {:<10} {}  {}  [{}]",
                                e.change,
                                e.kind,
                                e.key,
                                e.lines,
                                e.commits.join(" ")
                            )
                        }),
                );
            }
            out.extend(set.commits.iter().map(|c| {
                let checkpoint = c
                    .checkpoint
                    .map(|id| format!(" checkpoint event {id}"))
                    .unwrap_or_default();
                let session = c
                    .session
                    .as_ref()
                    .map(|s| format!(" session {s}"))
                    .unwrap_or_default();
                format!(
                    "  commit {} {}{checkpoint}{session} {}",
                    c.commit, c.author, c.subject
                )
            }));
            if let (Some(both), Some(ours)) = (&set.both, at.checked_sub(1).map(|i| &self[i])) {
                out.push(format!(
                    "changed on both sides (conflict candidates): {}",
                    both.len()
                ));
                let side = |set: &crate::ChangeSet, key: &str| {
                    set.entities
                        .iter()
                        .find(|e| e.key == key)
                        .map(|e| format!("{} [{}]", e.change, e.commits.join(" ")))
                        .unwrap_or_default()
                };
                out.extend(both.iter().map(|key| {
                    format!(
                        "  {key}  {}: {}  {}: {}",
                        ours.to,
                        side(ours, key),
                        set.to,
                        side(set, key)
                    )
                }));
            }
        }
        out.join("\n")
    }
}

impl Render for Vec<crate::Annotation> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no annotations".into();
        }
        self.iter()
            .map(|a| {
                let at: Vec<String> =
                    a.at.iter()
                        .map(|s| format!("{}: {}", s.rev, s.state))
                        .collect();
                format!(
                    "{:>4} {} {} {}  ({})",
                    a.id,
                    a.key,
                    a.kind,
                    a.value,
                    at.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for Vec<TestRow> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no test runs recorded; run legionctl code tests run".into();
        }
        self.iter()
            .map(|t| format!("{:<7} {} ({})  kills {}", t.result, t.name, t.file, t.kills))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for TestDetail {
    fn text(&self) -> String {
        let mut out = vec![format!(
            "{}: {} runs, {} mutants killed",
            self.name,
            self.runs.len(),
            self.mutants.len()
        )];
        out.extend(self.runs.iter().take(5).map(|r| {
            format!(
                "  run {} at {} ({}, {})",
                r.result, r.ran_at, r.file, r.git_ref
            )
        }));
        out.extend(self.mutants.iter().map(|m| {
            format!(
                "  killed {}:{} {} [{}]",
                m.file, m.line, m.description, m.source
            )
        }));
        out.extend(
            self.accepts
                .iter()
                .map(|a| format!("  accepted at {}: {}", a.accepted_at, a.reason)),
        );
        out.join("\n")
    }
}

impl Render for TestReview {
    fn text(&self) -> String {
        if self.tests.is_empty() {
            return "no new or changed test".into();
        }
        let mut out = Vec::new();
        for t in &self.tests {
            let state = match (&t.accepted, t.refused.is_empty()) {
                (Some(_), _) => "ACCEPTED",
                (None, true) => "ok",
                (None, false) => "REFUSED",
            };
            out.push(format!(
                "{state:<8} {} ({})  assertions {}, kills {}",
                t.name, t.file, t.assertions, t.kills
            ));
            out.extend(t.refused.iter().map(|r| format!("  refused: {r}")));
            out.extend(t.notes.iter().map(|n| format!("  note: {n}")));
            out.extend(t.accepted.iter().map(|a| format!("  accepted: {a}")));
        }
        out.join("\n")
    }

    /// False when the checkpoint gate refuses a test.
    fn success(&self) -> bool {
        self.tests
            .iter()
            .all(|t| t.accepted.is_some() || t.refused.is_empty())
    }
}

impl Render for crate::PlanCheck {
    fn text(&self) -> String {
        let mut out = vec![format!(
            "{} features at tree {}: {} problems",
            self.features,
            self.tree.get(..12).unwrap_or(&self.tree),
            self.problems.len()
        )];
        out.extend(self.problems.iter().map(|p| format!("  {p}")));
        out.join("\n")
    }

    fn success(&self) -> bool {
        self.problems.is_empty()
    }
}

impl Render for crate::PlanStatus {
    fn text(&self) -> String {
        let loc = |n: Option<u64>| n.map_or_else(|| "-".into(), |n| n.to_string());
        let mut out = vec![format!(
            "plan at {} (tree {})",
            self.rev,
            self.tree.get(..12).unwrap_or(&self.tree)
        )];
        for f in &self.features {
            out.push(format!(
                "{}  {}  {}  LOC {} of {} planned",
                f.id, f.status, f.title, f.loc_actual, f.loc_planned
            ));
            for w in &f.work_items {
                out.push(format!(
                    "  {} {} {}  LOC {} of {}  {}",
                    w.id,
                    w.status,
                    if w.complete { "complete" } else { "open" },
                    w.loc_actual,
                    w.loc_planned,
                    w.text
                ));
                out.extend(w.units.iter().map(|u| {
                    let by = u
                        .loc_by
                        .as_ref()
                        .map(|b| format!(" ({b})"))
                        .unwrap_or_default();
                    format!(
                        "    {:<11} {:<6} {}  LOC {}{by} of {}",
                        u.state,
                        u.change,
                        u.key,
                        loc(u.loc_actual),
                        loc(u.loc_planned)
                    )
                }));
                out.extend(w.tests.iter().map(|t| {
                    let state = if t.present { "present" } else { "missing" };
                    let run = t.result.as_deref().unwrap_or("not run");
                    format!("    {state:<11} test   {}  {run}, kills {}", t.key, t.kills)
                }));
                if let Some(since) = &self.since {
                    out.extend(
                        w.changed
                            .iter()
                            .map(|c| format!("    changed since {since}: {c}")),
                    );
                }
            }
        }
        out.extend(self.notes.iter().map(|n| format!("note: {n}")));
        out.join("\n")
    }
}

impl Render for Vec<crate::ThreadInfo> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no thread".into();
        }
        self.iter()
            .map(|t| {
                let from = t
                    .imported_from
                    .map_or_else(String::new, |f| format!(" (from thread {f})"));
                format!(
                    "{:>4} {:<7} {:<20} {:<12} {}{from}",
                    t.id,
                    t.state,
                    t.dossier,
                    t.account.as_deref().unwrap_or("-"),
                    t.title
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn message_line(m: &crate::ThreadMessage) -> String {
    let tool = m
        .tool
        .as_deref()
        .map(|t| format!(" [holds {t}]"))
        .unwrap_or_default();
    format!(
        "#{} {} {} -> {} ({}){tool}: {}",
        m.id, m.kind, m.from, m.to, m.state, m.body
    )
}

fn rule_text(r: &crate::WatchRule) -> String {
    format!(
        "rule {}: {} watches {}: {} on '{}' x{} without '{}' -> {}; run {}, misses {}, done for {} {}",
        r.id,
        r.member,
        r.watched,
        r.tools,
        r.path,
        r.every,
        r.expect,
        r.kind,
        r.run,
        r.misses,
        r.watched,
        r.done
    )
}

impl Render for Vec<crate::ThreadMessage> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no open message".into();
        }
        self.iter().map(message_line).collect::<Vec<_>>().join("\n")
    }
}

impl Render for Vec<crate::WatchRule> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no watch rule".into();
        }
        self.iter().map(rule_text).collect::<Vec<_>>().join("\n")
    }
}

impl Render for crate::ThreadView {
    fn text(&self) -> String {
        let t = &self.thread;
        let mut out = vec![format!(
            "thread {} ({}, dossier {}): {}",
            t.id, t.state, t.dossier, t.title
        )];
        out.push("members:".into());
        out.extend(self.members.iter().map(|m| {
            format!(
                "  {:<12} {:<16} {:<11} {:<7} session {}  wakes {}",
                m.name,
                m.archetype,
                m.pace,
                m.state,
                m.session.as_deref().unwrap_or("-"),
                m.wakes
            )
        }));
        if !self.rules.is_empty() {
            out.push("rules:".into());
            out.extend(self.rules.iter().map(|r| format!("  {}", rule_text(r))));
        }
        if !self.messages.is_empty() {
            out.push("messages:".into());
            out.extend(
                self.messages
                    .iter()
                    .map(|m| format!("  {}", message_line(m))),
            );
        }
        out.push(format!("log: {} rows (--json shows them)", self.log.len()));
        out.join("\n")
    }
}

fn peer_line(p: &crate::Peer) -> String {
    let place = match (&p.dossier, &p.topic) {
        (Some(d), Some(t)) if !t.is_empty() => format!("{d}--{t}"),
        (Some(d), _) => d.clone(),
        (None, _) => "-".into(),
    };
    let last = match (&p.last_tool, &p.last_path) {
        (Some(t), Some(path)) => format!("  last {t} {path}"),
        (Some(t), None) => format!("  last {t}"),
        _ => String::new(),
    };
    let declared = p
        .declared
        .as_deref()
        .map(|d| format!("  declared {d}"))
        .unwrap_or_default();
    let agent = match &p.subagent {
        Some(kind) => format!("{}/{kind}", p.agent),
        None => p.agent.clone(),
    };
    format!(
        "{:<38} {agent:<7} {:<8} {place}  pending {}  via {}  ({}){last}{declared}",
        p.session,
        p.state,
        p.pending,
        p.channels.join("+"),
        p.shares.join(", ")
    )
}

impl Render for Vec<crate::Peer> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no peer".into();
        }
        self.iter().map(peer_line).collect::<Vec<_>>().join("\n")
    }
}

impl Render for crate::Observed {
    fn text(&self) -> String {
        format!("{}\n{}", peer_line(&self.peer), self.events.text())
    }
}

/// Unicode bars, low to high, for a sparkline.
const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The values of a sparkline as bars between their minimum and maximum.
#[must_use]
pub fn sparkline(values: &[f64]) -> String {
    let low = values.iter().copied().fold(f64::INFINITY, f64::min);
    let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    values
        .iter()
        .map(|v| {
            let ratio = if high > low {
                (v - low) / (high - low)
            } else {
                0.0
            };
            BARS[(1..8u32)
                .filter(|i| f64::from(*i) <= ratio * 7.0 + 0.5)
                .count()]
        })
        .collect()
}

/// The rows of a table or list, by group when it has groups: `== group (count) ==` before each group's rows.
fn view_rows(v: &crate::ViewData, line: impl Fn(&[String]) -> String) -> Vec<String> {
    let mut out = Vec::new();
    if v.groups.is_empty() {
        out.extend(v.rows.iter().map(|r| line(r)));
        return out;
    }
    for group in &v.group_order {
        let rows: Vec<&Vec<String>> = v
            .rows
            .iter()
            .zip(&v.groups)
            .filter(|(_, g)| *g == group)
            .map(|(r, _)| r)
            .collect();
        out.push(format!("== {group} ({}) ==", rows.len()));
        out.extend(rows.into_iter().map(|r| line(r)));
    }
    out
}

/// A chart in one line per series: its points, range, last value and trend.
fn chart_text(v: &crate::ViewData) -> Vec<String> {
    std::iter::once(format!("{} series", v.series.len()))
        .chain(v.series.iter().map(|s| {
            let ys: Vec<f64> = s.points.iter().map(|p| p.1).collect();
            let (low, high) = ys
                .iter()
                .fold((f64::MAX, f64::MIN), |(l, h), y| (l.min(*y), h.max(*y)));
            format!(
                "{}: {} points, {low:.0} to {high:.0}, last {:.0} {}",
                if s.name.is_empty() { "-" } else { &s.name },
                s.points.len(),
                ys.last().copied().unwrap_or_default(),
                sparkline(&ys)
            )
        }))
        .collect()
}

/// A graph: its counts, its nodes two levels deep (counts below), its edges.
fn graph_text(v: &crate::ViewData) -> Vec<String> {
    {
        fn lines(nodes: &[crate::ViewNode], depth: usize, out: &mut Vec<String>) {
            for n in nodes {
                let count = n.count.map_or_else(String::new, |c| format!(" ({c})"));
                let mark = if n.marked { "* " } else { "" };
                let state = if n.state.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", n.state)
                };
                out.push(format!(
                    "{}{mark}{}{count} {}{state}",
                    "  ".repeat(depth),
                    n.label,
                    n.kind
                ));
                // two levels, counts below them (MEMORY-14: counts first)
                if depth < 1 {
                    lines(&n.children, depth + 1, out);
                }
            }
        }
        let marked = if v.legend.is_empty() {
            String::new()
        } else {
            format!("; * {}", v.legend)
        };
        let mut out = vec![format!(
            "{} nodes, {} edges{marked}",
            v.nodes.len(),
            v.edges.len()
        )];
        lines(&v.nodes, 0, &mut out);
        out.extend(
            v.edges
                .iter()
                .map(|e| format!("{} -{}-> {}", e.from, e.kind, e.to)),
        );
        out
    }
}

/// A form: its operation and fields (* required).
fn form_text(v: &crate::ViewData) -> Vec<String> {
    v.actions
        .iter()
        .filter_map(|b| crate::OPS.iter().find(|o| o.name == b.op))
        .map(|op| {
            let fields: Vec<String> = crate::form::fields(op)
                .into_iter()
                .map(|f| {
                    if f.required {
                        format!("{}*", f.name)
                    } else {
                        f.name
                    }
                })
                .collect();
            format!("form of {}: {}", op.name, fields.join(", "))
        })
        .collect()
}

/// A timeline: its count, then each event with its date.
fn timeline_text(v: &crate::ViewData) -> Vec<String> {
    std::iter::once(format!("{} events", v.rows.len()))
        .chain(v.rows.iter().map(|r| {
            let at = r[0]
                .parse::<i64>()
                .map_or_else(|_| r[0].clone(), legionctl_core::views::utc);
            format!("{at} {} {}: {}", r[1], r[2], r[3])
        }))
        .collect()
}

impl Render for crate::ViewData {
    /// The terminal form of a view (DESIGN-09 Rule 1): counts and the point first, one line each.
    fn text(&self) -> String {
        let head = format!("{} ({}, version {})", self.title, self.widget, self.version);
        if let Some(error) = &self.error {
            return format!("{head}: cannot read: {error}");
        }
        let body: Vec<String> = match self.widget {
            crate::ViewWidget::Table => std::iter::once(format!(
                "{} rows: {}",
                self.rows.len(),
                self.columns.join(" | ")
            ))
            .chain(view_rows(self, |r| r.join(" | ")))
            .collect(),
            crate::ViewWidget::List | crate::ViewWidget::Tree => {
                std::iter::once(format!("{} rows", self.rows.len()))
                    .chain(view_rows(self, |r| r.join(" ")))
                    .collect()
            }
            crate::ViewWidget::Gauge => vec![format!(
                "{} / {}",
                self.numbers.first().copied().unwrap_or_default(),
                self.max
            )],
            crate::ViewWidget::Sparkline => vec![sparkline(&self.numbers)],
            crate::ViewWidget::Markdown | crate::ViewWidget::Diff => vec![self.text.clone()],
            crate::ViewWidget::Stat => vec![format!(
                "{} {}{}{}{}",
                self.text,
                self.unit,
                if self.level.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", self.level)
                },
                if self.note.is_empty() {
                    String::new()
                } else {
                    format!(" - {}", self.note)
                },
                if self.numbers.len() > 1 {
                    format!(" {}", sparkline(&self.numbers))
                } else {
                    String::new()
                }
            )],
            crate::ViewWidget::Chart => chart_text(self),
            crate::ViewWidget::Stack => self.parts.iter().map(Render::text).collect(),
            crate::ViewWidget::Graph => graph_text(self),
            crate::ViewWidget::Form => form_text(self),
            crate::ViewWidget::Timeline => timeline_text(self),
        };
        let actions = self
            .actions
            .iter()
            .map(|a| format!("[{}] {} {}", a.label, a.op, a.input))
            .collect::<Vec<_>>();
        let mut out = vec![head];
        if self.widget != crate::ViewWidget::Stat && !self.note.is_empty() {
            out.push(self.note.clone());
        }
        out.extend(body);
        if !actions.is_empty() {
            out.push(format!("actions: {}", actions.join("; ")));
        }
        out.join("\n")
    }

    /// A view, or a view of its stack, that cannot read.
    fn success(&self) -> bool {
        self.error.is_none() && self.parts.iter().all(Render::success)
    }
}

impl Render for Vec<crate::ViewInfo> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no view".into();
        }
        self.iter()
            .map(|v| {
                let what = match (&v.widget, &v.error) {
                    (Some(w), _) => w.to_string(),
                    (None, Some(e)) => format!("not a view: {e}"),
                    (None, None) => String::new(),
                };
                format!(
                    "{:<24} {:<10} {what}  {}{}",
                    v.name,
                    v.layer,
                    v.title.as_deref().unwrap_or_default(),
                    v.intent
                        .as_deref()
                        .map_or_else(String::new, |i| format!(" (intent: {i})"))
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// `cpu-temp v2  stat of sensors $.[name=Package id 0].value  CPU temperature`, best fit first.
impl Render for Vec<crate::PartInfo> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no part".into();
        }
        self.iter()
            .map(|p| {
                format!(
                    "{} v{}  {} of {} {}  {}",
                    p.name, p.version, p.widget, p.op, p.path, p.title
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// One line per part of `about`: `daemon: legionctl-system ... (warn)`.
impl Render for Vec<crate::AboutItem> {
    fn text(&self) -> String {
        self.iter()
            .map(|i| {
                let warn = if i.state == "ok" { "" } else { " (needs you)" };
                format!("{}: {}{warn}", i.part, i.value)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// One line per step of `ask`: `composer: 3 runs, 2 answered, 1 failed; 9000 tokens in, 900 out; median 14000 ms`.
impl Render for Vec<crate::AskCost> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no ask yet".into();
        }
        self.iter()
            .map(|c| {
                format!(
                    "{}: {} runs, {} answered, {} failed; {} tokens in, {} out; median {} ms, longest {} ms",
                    c.step, c.runs, c.answered, c.failed, c.tokens_in, c.tokens_out, c.ms_median, c.ms_max
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// `#3 observer thread 1 observer0 openrouter (typesafe/jev-1.14) 169 tokens $0.0000 210 ms: action escalate 0.74
/// (ignore 0.01, note 0.24) confidence 0.62; acp ok; gate checkpoint passed`.
fn decision_line(d: &crate::RlcdDecision) -> String {
    let at = match (d.thread, &d.member) {
        (Some(t), Some(m)) => format!(" thread {t} {m}"),
        _ => String::new(),
    };
    let what = match (&d.answers, &d.error) {
        (Some(serde_json::Value::Object(answers)), _) => answers
            .iter()
            .map(|(id, a)| match a["type"].as_str() {
                Some("choice") => {
                    let choice = a["choice"].as_str().unwrap_or_default();
                    let p = |o: &str| a["probabilities"][o].as_f64().unwrap_or(0.0);
                    let others: Vec<String> = a["probabilities"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .filter(|(o, _)| *o != choice)
                        .map(|(o, _)| format!("{o} {:.2}", p(o)))
                        .collect();
                    let confidence = a["confidence"]
                        .as_f64()
                        .map_or_else(String::new, |c| format!(" confidence {c:.2}"));
                    format!(
                        "{id} {choice} {:.2} ({}){confidence}",
                        p(choice),
                        others.join(", ")
                    )
                }
                Some("yesno") => format!("{id} yes {:.2}", a["yes"].as_f64().unwrap_or(0.0)),
                _ => format!("{id} score {:.2}", a["score"].as_f64().unwrap_or(0.0)),
            })
            .collect::<Vec<_>>()
            .join("; "),
        (_, Some(e)) => format!("failed: {e}"),
        _ => "no answer".into(),
    };
    let acp = match &d.acp {
        Some(a) if a["error"].is_string() => {
            format!("; acp failed: {}", a["error"].as_str().unwrap_or_default())
        }
        Some(_) => "; acp answered".into(),
        None => String::new(),
    };
    let gate = d.gate.as_ref().map_or_else(String::new, |g| {
        format!(
            "; gate {} {}",
            g["gate"].as_str().unwrap_or_default(),
            if g["passed"] == true {
                "passed"
            } else {
                "held"
            }
        )
    });
    format!(
        "#{} {}{at} {} ({}) {} tokens ${:.4} {} ms: {what}{acp}{gate}",
        d.id, d.path, d.backend, d.model, d.input_tokens, d.cost_usd, d.ms
    )
}

impl Render for crate::RlcdDecision {
    fn text(&self) -> String {
        decision_line(self)
    }
}

impl Render for Vec<crate::RlcdDecision> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no rlcd decision".into();
        }
        self.iter()
            .map(decision_line)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// One fact in one line: `* #id entity attribute = value (scope, source[, state])`; `*` marks a winner.
fn fact_line(f: &crate::Fact) -> String {
    let mark = if f.wins { "*" } else { " " };
    let scope = if f.scope_id.is_empty() {
        f.scope.to_string()
    } else {
        format!("{} {}", f.scope, f.scope_id)
    };
    let state = if f.state.is_empty() || f.state == "holds" {
        String::new()
    } else {
        format!(", {}", f.state)
    };
    let refs = match (&f.from_ref, &f.to_ref) {
        (None, None) => String::new(),
        (from, to) => format!(
            ", refs {}..{}",
            from.as_deref().map_or("", |c| &c[..c.len().min(8)]),
            to.as_deref().map_or("", |c| &c[..c.len().min(8)])
        ),
    };
    let recorded = if f.recorded_to.is_some() {
        ", corrected"
    } else {
        ""
    };
    format!(
        "{mark} #{} {} {} = {} ({scope}, {}{refs}{state}{recorded})",
        f.id, f.entity, f.attribute, f.value, f.source
    )
}

impl Render for Vec<crate::Fact> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no fact".into();
        }
        self.iter().map(fact_line).collect::<Vec<_>>().join("\n")
    }
}

impl Render for Vec<crate::FactChange> {
    fn text(&self) -> String {
        if self.is_empty() {
            return "no change".into();
        }
        let shown =
            |v: &Option<serde_json::Value>| v.as_ref().map_or("-".into(), ToString::to_string);
        self.iter()
            .map(|c| {
                format!(
                    "{} {}: {} -> {}",
                    c.entity,
                    c.attribute,
                    shown(&c.before),
                    shown(&c.after)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for Vec<crate::OpEntry> {
    fn text(&self) -> String {
        std::iter::once(format!("{} operations", self.len()))
            .chain(self.iter().map(|o| {
                format!(
                    "{:<28} {:<9} {}{}",
                    o.name,
                    format!("{:?}", o.class).to_lowercase(),
                    o.summary,
                    if o.dry_run { " (dry run)" } else { "" }
                )
            }))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for crate::CodeTree {
    fn text(&self) -> String {
        let at = if self.prefix.is_empty() {
            "."
        } else {
            &self.prefix
        };
        std::iter::once(format!(
            "{at}: {} symbols in {} entries (tree {})",
            self.total,
            self.items.len(),
            self.tree
        ))
        .chain(self.items.iter().map(|i| {
            if i.symbol {
                format!("  {:<10} {}  line {}", i.kind, i.path, i.line.unwrap_or(0))
            } else {
                format!("  {:<6} {:>6}  {}", i.kind, i.count, i.path)
            }
        }))
        .collect::<Vec<_>>()
        .join("\n")
    }
}

impl Render for crate::ViewAnswer {
    fn text(&self) -> String {
        let (tokens_in, tokens_out, ms) = self.steps.iter().fold((0, 0, 0), |(i, o, m), s| {
            (i + s.tokens_in, o + s.tokens_out, m + s.ms)
        });
        let cost = if tokens_in > 0 {
            format!(", {tokens_in} tokens in, {tokens_out} out, {ms} ms")
        } else {
            String::new()
        };
        let mut out = vec![format!(
            "{} ({}{cost})",
            self.view.as_deref().unwrap_or("no view"),
            self.route,
        )];
        if !self.text.trim().is_empty() {
            out.push(self.text.trim().to_string());
        }
        if let Some(error) = &self.error {
            out.push(format!("the view failed its check: {error}"));
        }
        if let Some(data) = &self.data {
            out.push(data.text());
        }
        out.join("\n")
    }

    fn success(&self) -> bool {
        self.error.is_none() && self.data.as_ref().is_none_or(Render::success)
    }
}
