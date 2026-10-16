//! A stand-in keyring for the tests: never the user's.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `PATH` with a stand-in `secret-tool` first, which keeps entries as files in `home/keyring` (`<attribute value
/// with / as _>`), then `rest`.
pub fn keyring_path(home: &Path, rest: &OsString) -> OsString {
    let bin: PathBuf = home.join("keyring-bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        bin.join("secret-tool"),
        format!(
            "#!/bin/sh\nk={}; mkdir -p \"$k\"\ncase \"$1\" in\n  lookup) cat \"$k/$(printf %s \"$3\" | tr / _)\" 2>/dev/null || exit 1;;\n  store) cat > \"$k/$(printf %s \"$4\" | tr / _)\";;\nesac\n",
            home.join("keyring").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        bin.join("secret-tool"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(rest))).unwrap()
}
