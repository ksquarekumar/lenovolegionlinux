//! What the machine daemon does for each event. The same decisions the Python units made, now in one place and in
//! one queue, so two events never race.

use anyhow::Result;

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
    /// Periodic check for knobs that cannot notify (power-profiles-daemon rewrites governor and EPP).
    Tick,
}

pub fn react(machine: &Machine, event: Event) -> Result<Vec<Step>> {
    Ok(match event {
        Event::Start | Event::PowerSource => machine.apply(None, Options::default())?.1,
        Event::Resume => {
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
