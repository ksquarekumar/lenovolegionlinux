//! What the machine daemon does for each event, in one place and in one queue, so two events never race.

use anyhow::Result;

use crate::fan;
use crate::policy::{Machine, Options, Source, Step};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The daemon started (boot, restart, upgrade).
    Start,
    /// The power adapter was plugged or unplugged.
    PowerSource,
    /// `platform_profile` changed: our own write, or Fn+Q.
    PlatformProfile,
    /// The machine woke from suspend.
    Resume,
    /// A configset layer changed (inotify on the folders, D-Bus `Reload`, `rules`).
    Reload,
    /// A workload (game, build, vm) started or ended (the exec sensor).
    Workload,
    /// Periodic check for knobs that cannot notify (power-profiles-daemon rewrites governor and EPP).
    Tick,
}

/// Plan the reaction to `event`.
///
/// # Errors
/// The plan for the event fails (see [`Machine::apply`] and [`Machine::follow`]).
pub fn react(machine: &Machine, event: Event) -> Result<Vec<Step>> {
    let mut steps = policy(machine, event)?;
    // the firmware forgets fan max mode in suspend and the fan table outside custom mode
    if matches!(event, Event::Start | Event::Resume | Event::PlatformProfile) {
        let again = fan::reassert(machine, event != Event::PlatformProfile, &steps);
        steps.extend(again);
    }
    Ok(steps)
}

fn policy(machine: &Machine, event: Event) -> Result<Vec<Step>> {
    Ok(match event {
        Event::Start | Event::PowerSource => machine.apply(None, Options::default())?.1,
        // the same apply as after a power change, but the Fn+Q choice stays
        Event::Resume | Event::Reload | Event::Workload => {
            machine
                .apply(
                    None,
                    Options {
                        keep_override: true,
                        ..Options::default()
                    },
                )?
                .1
        }
        Event::PlatformProfile => machine.follow()?,
        Event::Tick if machine.ac_online() => {
            machine
                .apply(
                    Some(Source::Ac),
                    Options {
                        reassert: true,
                        ..Options::default()
                    },
                )?
                .1
        }
        Event::Tick => vec![],
    })
}
