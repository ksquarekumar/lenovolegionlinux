//! The ACP adapter as the defaults run Zed's: a `#!/usr/bin/env node` script that bun runs
//! (`bun --bun x --no-install <adapter>`). The script runs the stand-in agent `tests/fixtures/fake-acp-agent.py`.

use std::path::Path;

/// The ACP command of the defaults for adapter `fake-acp`.
pub const COMMAND: [&str; 5] = ["bun", "--bun", "x", "--no-install", "fake-acp"];

/// The adapter script, and a `node` in ~/.local/bin of `home` that fails: the script runs only under bun's runtime.
pub fn node_script(home: &Path) -> String {
    let agent =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    executable(
        &home.join(".local/bin/node"),
        "#!/bin/sh\necho 'node ran' >&2\nexit 97\n",
    );
    format!(
        "#!/usr/bin/env node\nconst r = require('node:child_process').spawnSync('python3', [{}], \
         {{stdio: 'inherit'}});\nprocess.exit(r.status ?? 1);\n",
        serde_json::json!(agent)
    )
}

/// `text` at `path`, executable.
pub fn executable(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// A stand-in bun in ~/.bun/bin of `home`, where legionctl looks first. `pm -g bin` and `add -g` work in
/// `$BUN_INSTALL` as bun 1.x does (no global folder before the first `add -g`); `add -g` logs to ~/bun.log, takes one
/// second, and installs the [`node_script`] adapter as `fake-acp` (linked into the package's folder, as bun does), or fails as an unreachable registry while the file
/// ~/install-fails is there. `--bun x` runs the real bun.
pub fn stand_in_bun(home: &Path) {
    let adapter = node_script(home);
    std::fs::write(home.join("adapter.js"), adapter).unwrap();
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("bun"))
        .find(|p| p.is_file())
        .expect("bun on PATH");
    executable(
        &home.join(".bun/bin/bun"),
        &format!(
            r#"#!/bin/sh
case "$1 $2" in
"pm -g")
  [ -f "$BUN_INSTALL/install/global/package.json" ] || {{ echo "error: No package.json was found for directory \"$BUN_INSTALL/install/global\"" >&2; exit 1; }}
  echo "$BUN_INSTALL/bin" ;;
"add -g")
  echo "$*" >> "$HOME/bun.log"
  [ -e "$HOME/install-fails" ] && {{ echo "error: GET https://registry.npmjs.org/$3 - 503" >&2; exit 1; }}
  sleep 1
  mkdir -p "$BUN_INSTALL/bin" "$BUN_INSTALL/install/global"
  touch "$BUN_INSTALL/install/global/package.json"
  dir="$BUN_INSTALL/install/global/node_modules/$3"; mkdir -p "$dir"
  cp "$HOME/adapter.js" "$dir/index.js"; chmod +x "$dir/index.js"
  ln -sfn "$dir/index.js" "$BUN_INSTALL/bin/fake-acp" ;;
"--bun x") exec {} "$@" ;;
*) exit 64 ;;
esac
"#,
            real.display()
        ),
    );
}

/// The `add -g` calls of the stand-in bun.
pub fn installs(home: &Path) -> Vec<String> {
    std::fs::read_to_string(home.join("bun.log"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}
