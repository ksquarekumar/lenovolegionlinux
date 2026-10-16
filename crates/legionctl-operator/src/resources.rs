//! `resources.list` and the routes of `resources.act` (note 22): each resource of the machine with its state, what
//! uses it, and its actions. An action is only a route, an operation and its input; the server runs that operation as
//! a call of its own, so no action has a second implementation here.

use anyhow::{Context, Result};
use legionctl_api::{
    Acted, Code, Ctx, DesktopGpu, Empty, Engine, GpuOwner, OPS, ProfileIn, Resource, ResourceActIn,
    ResourceAction, ResourceList, coded,
};
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::{mounts, sets, winvm};
use serde_json::{Value, json};

use crate::engine::{Operator, nvidia_owner, unit_active, windows_qmp};

/// The action `name` runs `op` with `input`; its class and dry run are the operation's own.
fn action(name: &str, op: &str, input: Value) -> Result<ResourceAction> {
    let info = OPS
        .iter()
        .find(|o| o.name == op)
        .with_context(|| format!("the route of {name} names no operation {op}"))?;
    Ok(ResourceAction {
        name: name.into(),
        op: op.into(),
        input,
        class: info.class,
        dry_run: (info.input)()["properties"].get("dry_run").is_some(),
    })
}

fn resource(
    group: &str,
    id: String,
    name: String,
    state: String,
    users: Vec<String>,
    actions: Vec<ResourceAction>,
) -> Resource {
    Resource {
        group: group.into(),
        id,
        name,
        state,
        users,
        actions,
    }
}

/// The installed Windows in a VM (when this machine has a Windows disk), and each container and VM of `virt status`.
fn vm(op: &Operator, ctx: &Ctx, root: &SysRoot) -> Result<Vec<Resource>> {
    let mut out = Vec::new();
    if winvm::windows_disk(root)?.is_some() {
        let unit = format!("{}.service", legionctl_core::session::WINDOWS_UNIT);
        let state = if !unit_active(&unit) {
            "stopped"
        } else if windows_qmp()
            .and_then(|mut q| q.execute("query-status", None))
            .is_ok_and(|s| s["status"] == "paused")
        {
            "paused"
        } else {
            "running"
        };
        let actions = match state {
            "stopped" => vec![action("start", "windows.vm.start", json!({}))?],
            "paused" => vec![
                action("resume", "windows.vm.resume", json!({}))?,
                action("stop", "windows.vm.stop", json!({}))?,
            ],
            _ => vec![
                action("pause", "windows.vm.pause", json!({}))?,
                action("stop", "windows.vm.stop", json!({}))?,
            ],
        };
        out.push(resource(
            "vm",
            "vm:windows".into(),
            "Windows (installed, in a VM)".into(),
            state.into(),
            Vec::new(),
            actions,
        ));
    }
    for i in op.virt_status(ctx, Empty {})?.instances {
        let virt = |name: &str, act: &str| {
            action(name, "virt.act", json!({"action": act, "name": i.name}))
        };
        let actions = match i.state.as_str() {
            "running" => vec![virt("pause", "pause")?, virt("stop", "stop")?],
            "paused" => vec![virt("resume", "unpause")?, virt("stop", "stop")?],
            _ => vec![virt("start", "start")?],
        };
        out.push(resource(
            "vm",
            format!("virt:{}", i.name),
            format!("{} ({})", i.name, i.image),
            i.state.clone(),
            i.consumer.into_iter().collect(),
            actions,
        ));
    }
    Ok(out)
}

/// The NVIDIA GPU's owner (when the machine has one) and the GPU that draws the desktop.
fn gpu(op: &Operator, ctx: &Ctx, root: &SysRoot) -> Result<Vec<Resource>> {
    let mut out = Vec::new();
    if let Ok((_, owner)) = nvidia_owner(root) {
        let name = |o: GpuOwner| match o {
            GpuOwner::Linux => "linux",
            GpuOwner::Windows => "windows",
            GpuOwner::None => "none",
        };
        let actions = [GpuOwner::Linux, GpuOwner::Windows, GpuOwner::None]
            .into_iter()
            .filter(|o| *o != owner)
            .map(|o| {
                action(
                    &format!("give-{}", name(o)),
                    "gpu.give",
                    json!({"gpu": "nvidia", "to": o}),
                )
            })
            .collect::<Result<_>>()?;
        out.push(resource(
            "gpu",
            "gpu:nvidia".into(),
            "NVIDIA GPU".into(),
            name(owner).into(),
            // the list's count of processes not visible comes from the mount scan of the same processes
            mounts::users_of(root, "/dev/nvidia")?.0,
            actions,
        ));
    }
    let desktop = op.gpu_status(ctx, Empty {})?.desktop_gpu;
    let actions = [DesktopGpu::Nvidia, DesktopGpu::Intel, DesktopGpu::Auto]
        .into_iter()
        .filter(|c| *c != desktop)
        .map(|c| {
            action(
                &c.to_string(),
                "gpu.primary",
                json!({"choice": c, "release_nvidia": false}),
            )
        })
        .collect::<Result<_>>()?;
    out.push(resource(
        "gpu",
        "desktop-gpu".into(),
        "The GPU that draws the desktop (next login)".into(),
        desktop.to_string(),
        Vec::new(),
        actions,
    ));
    Ok(out)
}

/// Each mount with what uses it, and the number of processes whose files the reader may not see; the Windows disk's
/// partitions attach and detach.
fn disk(root: &SysRoot) -> Result<(Vec<Resource>, usize)> {
    // no Windows disk: no partition to attach or detach
    let windows = winvm::disk_targets(root)?.unwrap_or_default();
    let (mounts, not_visible) = mounts::list(root)?;
    let resources = mounts
        .into_iter()
        .map(|m| {
            let disk = |name: &str| action(name, "windows.disk", json!({"action": name}));
            let actions = match (windows.contains(&m.target), m.mounted) {
                (false, _) => Vec::new(),
                (true, true) => vec![disk("detach")?],
                (true, false) => vec![disk("attach")?, disk("detach")?],
            };
            Ok(resource(
                "disk",
                format!("mount:{}", m.target.display()),
                format!("{} ({})", m.source, m.fstype),
                if m.mounted { "mounted" } else { "not mounted" }.into(),
                m.users,
                actions,
            ))
        })
        .collect::<Result<_>>()?;
    Ok((resources, not_visible))
}

/// The configset layers, the held knobs and the saved sets.
fn configset(op: &Operator, ctx: &Ctx, root: &SysRoot) -> Result<Vec<Resource>> {
    let mut out = Vec::new();
    for rule in op.rules_list(ctx, Empty {})?.layers {
        let layer = json!({"layer": rule.id});
        let (state, actions) = match (&rule.layer.off, rule.layer.failed) {
            (None, _) => (
                if rule.active { "on, used now" } else { "on" }.to_string(),
                vec![action("disable", "rules.disable", layer)?],
            ),
            (Some(why), false) => (
                format!("off: {why}"),
                vec![action("enable", "rules.enable", layer)?],
            ),
            (Some(why), true) => (format!("off: {why}"), Vec::new()),
        };
        out.push(resource(
            "configset",
            format!("layer:{}", rule.id),
            rule.layer.file.clone(),
            state,
            Vec::new(),
            actions,
        ));
    }
    for knob in op.knobs_list(ctx, ProfileIn { profile: None })? {
        let Some(held) = knob.held else { continue };
        out.push(resource(
            "configset",
            format!("knob:{}", knob.id),
            knob.path.clone(),
            format!("held at {held}"),
            Vec::new(),
            vec![action(
                "release",
                "knobs.release",
                json!({"knob": knob.id}),
            )?],
        ));
    }
    let ledger = legionctl_machine::ledger::Ledger::read(root)?;
    for (name, set) in ledger
        .sets()?
        .into_iter()
        .filter(|(n, _)| n != sets::BEFORE_BOOT)
    {
        let mut actions = vec![action(
            "switch",
            "rules.sets.switch",
            json!({"name": name}),
        )?];
        if name != sets::PREVIOUS {
            actions.push(action(
                "remove",
                "rules.sets.remove",
                json!({"name": name}),
            )?);
        }
        out.push(resource(
            "configset",
            format!("set:{name}"),
            format!(
                "{} layer states, {} holds",
                set.layers.len(),
                set.holds.len()
            ),
            if set.boot {
                "saved, with boot entries"
            } else {
                "saved"
            }
            .into(),
            Vec::new(),
            actions,
        ));
    }
    Ok(out)
}

/// Each agent session, and each workspace with the sessions that work in it.
fn session(op: &Operator, ctx: &Ctx) -> Result<Vec<Resource>> {
    let sessions = op.session_list(ctx, Empty {})?;
    let mut out = Vec::new();
    for s in &sessions {
        let id = json!({"id": s.session.id});
        let (state, actions) = match s.state.as_str() {
            "active" => (
                "running",
                vec![
                    action("pause", "session.pause", id.clone())?,
                    action("stop", "session.stop", id)?,
                ],
            ),
            "frozen" => (
                "paused",
                vec![
                    action("resume", "session.unpause", id.clone())?,
                    action("stop", "session.stop", id)?,
                ],
            ),
            _ => ("ended", vec![action("start", "session.resume", id)?]),
        };
        out.push(resource(
            "session",
            format!("session:{}", s.session.id),
            format!("{} in {}", s.session.agent, s.session.dossier),
            state.into(),
            vec![s.session.workdir.display().to_string()],
            actions,
        ));
    }
    let agents = crate::engine::config(None)?.config.session.agents;
    for w in op.workspace_list(ctx, Empty {})? {
        let users = sessions
            .iter()
            .filter(|s| s.state != "inactive" && s.session.workdir.starts_with(&w.path))
            .map(|s| format!("session {}", s.session.id))
            .collect();
        let actions = agents
            .keys()
            .map(|agent| {
                action(
                    &format!("start-{agent}"),
                    "session.start",
                    json!({"agent": agent, "dossier": w.dossier, "repo": w.path}),
                )
            })
            .collect::<Result<_>>()?;
        out.push(resource(
            "session",
            format!("workspace:{}", w.path.display()),
            format!("{} ({})", w.dossier, w.branch),
            w.branch.clone(),
            users,
            actions,
        ));
    }
    Ok(out)
}

/// Every resource, group by group.
pub fn list(op: &Operator, ctx: &Ctx) -> Result<ResourceList> {
    let root = SysRoot::from_env();
    let mut resources = vm(op, ctx, &root)?;
    resources.extend(gpu(op, ctx, &root)?);
    let (disks, not_visible) = disk(&root)?;
    resources.extend(disks);
    resources.extend(configset(op, ctx, &root)?);
    resources.extend(session(op, ctx)?);
    Ok(ResourceList {
        resources,
        not_visible,
    })
}

/// The route of an action: its operation and input, from the same list. A dry run of an operation with its own dry
/// run runs it with `dry_run`; of one without, it does not run.
pub fn route(op: &Operator, ctx: &Ctx, input: &ResourceActIn) -> Result<Acted> {
    let all = list(op, ctx)?.resources;
    let r = all.iter().find(|r| r.id == input.id).ok_or_else(|| {
        let ids: Vec<&str> = all.iter().map(|r| r.id.as_str()).collect();
        coded(
            Code::NotFound,
            format!(
                "no resource {}; the resources: {}",
                input.id,
                ids.join(", ")
            ),
        )
    })?;
    let a = r
        .actions
        .iter()
        .find(|a| a.name == input.action)
        .ok_or_else(|| {
            let names: Vec<&str> = r.actions.iter().map(|a| a.name.as_str()).collect();
            coded(
                Code::InvalidInput,
                format!(
                    "{} ({}) has no action {}; its actions now: {}",
                    r.id,
                    r.state,
                    input.action,
                    if names.is_empty() {
                        "none".into()
                    } else {
                        names.join(", ")
                    }
                ),
            )
        })?;
    let mut routed = a.input.clone();
    if input.dry_run && a.dry_run {
        routed["dry_run"] = true.into();
    }
    Ok(Acted {
        op: a.op.clone(),
        input: routed,
        run: !input.dry_run || a.dry_run,
        output: None,
    })
}
