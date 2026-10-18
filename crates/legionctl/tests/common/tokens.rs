//! Stand-in session tokens for the tests that act as a sandboxed session.

use std::path::Path;
use std::process::Command;

/// A session's token in the operator database of `home`, as `session start` issues it (only its hash is kept), and
/// its grant: the toolsets of its own session, of your settings and of the brokered MCP tools, as a started session's
/// skills give them.
pub fn add_session_token(home: &Path, token: &str, session: &str) {
    let added = Command::new("python3")
        .args([
            "-c",
            "import hashlib, json, sqlite3, sys; c = sqlite3.connect(sys.argv[1]); s = sys.argv[3]; \
             c.execute('INSERT INTO tokens VALUES (?, ?, 0)', \
             (hashlib.blake2b(sys.argv[2].encode(), digest_size=32).hexdigest(), s)); \
             c.execute(\"INSERT INTO sessions (id, agent, dossier, workdir, started) VALUES (?, 'probe', '', '', 0)\", \
             (s,)); \
             grant = {'runnable': 'session:' + s, 'requested': ['session-own', 'settings-change', 'mcp-tools'], \
                      'sets': [], 'manifest': {'name': 'session:' + s, \
                      'toolverse': {'classes': ['session', 'workspace'], 'mcp_tools': ['*/*']}}}; \
             c.execute(\"INSERT INTO facts (entity, attribute, value, scope, scope_id, source, evidence, confidence, \
                        valid_from, recorded_from) VALUES (?, 'manifest', ?, 'global', '', 'rule', '{}', 1.0, 0, 0)\", \
                       ('runnable:session:' + s, json.dumps(grant))); c.commit()",
        ])
        .arg(home.join(".local/state/legionctl/operator.db"))
        .args([token, session])
        .status()
        .unwrap();
    assert!(added.success());
}
