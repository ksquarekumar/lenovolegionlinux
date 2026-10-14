//! The configset folders, watched with inotify (note 01): a change of a layer file, or of the user in front (logind's
//! seat0 file), applies the layers again.

use std::path::PathBuf;

use anyhow::{Context, Result};
use inotify::{Inotify, WatchDescriptor, WatchMask};
use legionctl_machine::configset;
use legionctl_machine::sysroot::SysRoot;
use tokio::io::unix::AsyncFd;

/// logind writes a seat's file anew (a rename) when its active session changes.
const SEATS: &str = "/run/systemd/seats";

fn mask() -> WatchMask {
    WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO | WatchMask::MOVED_FROM | WatchMask::DELETE
}

pub struct Folders {
    root: SysRoot,
    fd: AsyncFd<Inotify>,
    /// How often to look for another user in front, or a folder that appeared.
    look: tokio::time::Interval,
    /// The folder of the user in front, while it exists.
    user: Option<(PathBuf, WatchDescriptor)>,
}

impl Folders {
    /// Watch the shipped layers of the profile, the generated and the system folder (made when missing) and the
    /// seats; a folder that cannot be watched is reported, and the others are watched.
    ///
    /// # Errors
    /// The profile cannot be read, or inotify cannot start.
    pub fn watch(root: &SysRoot) -> Result<Folders> {
        let (profile, _) = legionctl_machine::profile::load(None)?;
        let inotify = Inotify::init().context("starting inotify")?;
        for dir in [configset::GENERATED, configset::SYSTEM] {
            if let Err(e) = std::fs::create_dir_all(root.path(dir)) {
                eprintln!("configsets: cannot make {dir}: {e}");
            }
        }
        let shipped = format!("{}/{}", configset::SHIPPED, profile.id);
        for dir in [
            shipped.as_str(),
            configset::GENERATED,
            configset::SYSTEM,
            SEATS,
        ] {
            if let Err(e) = inotify.watches().add(root.path(dir), mask()) {
                eprintln!("configsets: {dir} is not watched: {e}");
            }
        }
        let mut folders = Folders {
            root: root.clone(),
            fd: AsyncFd::new(inotify)?,
            look: tokio::time::interval(std::time::Duration::from_secs(30)),
            user: None,
        };
        folders.follow_user();
        Ok(folders)
    }

    /// Watch the layer folder of the user in front now (the user changes; the folder can appear later).
    fn follow_user(&mut self) {
        let wanted = configset::active_user(&self.root)
            .map(|(_, home)| self.root.path(configset::user_folder(&home)))
            .filter(|dir| dir.is_dir());
        if wanted.as_ref() == self.user.as_ref().map(|(dir, _)| dir) {
            return;
        }
        if let Some((_, wd)) = self.user.take() {
            // the folder may be gone, and its watch with it
            let _ = self.fd.get_ref().watches().remove(wd);
        }
        if let Some(dir) = wanted {
            match self.fd.get_ref().watches().add(&dir, mask()) {
                Ok(wd) => self.user = Some((dir, wd)),
                Err(e) => eprintln!("configsets: {} is not watched: {e}", dir.display()),
            }
        }
    }

    /// Wait until a watched folder changed; the events that came together count once. Meanwhile, and after each
    /// change, it follows the user in front ([`Folders::follow_user`]).
    ///
    /// # Errors
    /// The inotify descriptor fails.
    pub async fn changed(&mut self) -> Result<()> {
        let mut buffer = [0u8; 4096];
        loop {
            let seen = tokio::select! {
                ready = self.fd.readable_mut() => {
                    let mut guard = ready?;
                    // everything there now (an editor's write and rename) is one change
                    let mut seen = 0;
                    loop {
                        match guard.get_inner_mut().read_events(&mut buffer) {
                            Ok(events) => seen += events.count(),
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(e) => return Err(e).context("reading inotify events"),
                        }
                    }
                    guard.clear_ready();
                    seen
                }
                _ = self.look.tick() => 0,
            };
            self.follow_user();
            if seen > 0 {
                return Ok(());
            }
        }
    }
}
