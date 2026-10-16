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
    /// A change came and the folders have not settled yet.
    pending: bool,
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
            pending: false,
        };
        folders.follow_user();
        Ok(folders)
    }

    /// Watch the layer folder of the user in front now (the user changes; the folder can appear later).
    fn follow_user(&mut self) {
        let wanted = match configset::active_user(&self.root) {
            Ok(user) => user
                .map(|(_, home)| self.root.path(configset::user_folder(&home)))
                .filter(|dir| dir.is_dir()),
            Err(e) => {
                // the watch stays as it is until the user can be read again
                eprintln!("configsets: the user in front cannot be read: {e:#}");
                return;
            }
        };
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

    /// Wait until a watched folder changed and then stayed as it is for [`SETTLE`]: the events until then count once,
    /// so a package upgrade that replaces the files one by one applies once, with all of them (todo-125). Meanwhile,
    /// and after each change, it follows the user in front ([`Folders::follow_user`]).
    ///
    /// # Errors
    /// The inotify descriptor fails.
    pub async fn changed(&mut self) -> Result<()> {
        // the main loop drops this future when another event comes first: `pending` keeps a change seen until then
        while !self.pending {
            let seen = tokio::select! {
                ready = self.fd.readable_mut() => drain(ready?)?,
                _ = self.look.tick() => 0,
            };
            self.follow_user();
            self.pending = seen > 0;
        }
        while let Ok(ready) = tokio::time::timeout(SETTLE, self.fd.readable_mut()).await {
            drain(ready?)?;
        }
        self.follow_user();
        self.pending = false;
        Ok(())
    }
}

/// How long the folders stay unchanged before a change applies.
const SETTLE: std::time::Duration = std::time::Duration::from_secs(1);

/// The number of inotify events there now (an editor's write and rename are one change).
fn drain(mut guard: tokio::io::unix::AsyncFdReadyMutGuard<'_, Inotify>) -> Result<usize> {
    let mut buffer = [0u8; 4096];
    let mut seen = 0;
    loop {
        match guard.get_inner_mut().read_events(&mut buffer) {
            Ok(events) => seen += events.count(),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => return Err(e).context("reading inotify events"),
        }
    }
    guard.clear_ready();
    Ok(seen)
}
