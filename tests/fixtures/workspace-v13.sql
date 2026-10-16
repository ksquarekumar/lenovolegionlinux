-- ~/.agentworkspace/workspace.db as P5 left it (v13: threads, kills not keyed), for the migration test
-- one open binding per dossier and per session; released rows stay as history; place: where the session worked
-- (repository root, else its folder), for binding the next session there
     CREATE TABLE bindings (
        session  TEXT NOT NULL,
        tool     TEXT NOT NULL,
        dossier  TEXT NOT NULL,
        acquired INTEGER NOT NULL,
        updated  INTEGER NOT NULL,
        released INTEGER,
        place    TEXT
     );
     CREATE UNIQUE INDEX open_dossier ON bindings (dossier) WHERE released IS NULL;
     CREATE UNIQUE INDEX open_session ON bindings (session) WHERE released IS NULL;
     CREATE TABLE writes (
        ts      INTEGER NOT NULL,
        session TEXT NOT NULL,
        kind    TEXT NOT NULL,
        path    TEXT NOT NULL
     );
-- workspaces, the operating directories of dossiers (one open per dossier, repository and topic; '' is the
-- dossier's own workspace)
     CREATE TABLE workspaces (
        dossier TEXT NOT NULL,
        repo    TEXT NOT NULL,
        path    TEXT NOT NULL,
        branch  TEXT NOT NULL,
        base    TEXT NOT NULL,
        opened  INTEGER NOT NULL,
        closed  INTEGER,
        topic   TEXT NOT NULL DEFAULT ''
     );
     CREATE UNIQUE INDEX open_workspace ON workspaces (dossier, repo, topic) WHERE closed IS NULL;
-- sessions that legionctl started (note 14): the account profile (A1); scoped: in its own systemd scope, which
-- then tells whether it is still live; inside: host (a bwrap sandbox), container or vm (W5)
     CREATE TABLE sessions (
        id      TEXT PRIMARY KEY,
        agent   TEXT NOT NULL,
        dossier TEXT NOT NULL,
        workdir TEXT NOT NULL,
        started INTEGER NOT NULL,
        account TEXT,
        scoped  INTEGER NOT NULL DEFAULT 0,
        inside  TEXT NOT NULL DEFAULT 'host'
     );
-- status history of the work graph's nodes, from writes through legionctl (W4)
     CREATE TABLE status_changes (
        ts          INTEGER NOT NULL,
        session     TEXT NOT NULL,
        dossier     TEXT NOT NULL,
        node        TEXT NOT NULL,
        from_status TEXT,
        to_status   TEXT
     );
-- dossier nudges: per session, the workspace content at the last reply, the replies that changed it since the
-- session last wrote its dossier, and the nudge level the agent was last told
     CREATE TABLE nudges (
        session TEXT PRIMARY KEY,
        work    TEXT,
        turns   INTEGER NOT NULL DEFAULT 0,
        level   TEXT
     );
-- annotations on stable code keys (design 23, A6): the tree and the symbol's content hash when written
     CREATE TABLE annotations (
        id           INTEGER PRIMARY KEY,
        repo         TEXT NOT NULL,
        key          TEXT NOT NULL,
        kind         TEXT NOT NULL,
        value        TEXT NOT NULL,
        tree         TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        session      TEXT NOT NULL,
        agent        TEXT NOT NULL,
        created      INTEGER NOT NULL
     );
     CREATE INDEX annotations_key ON annotations (repo, key);
-- one row per test per run of `code tests run` (A7): file is the source of the test binary
     CREATE TABLE test_runs (
        repo   TEXT NOT NULL,
        ref    TEXT NOT NULL,
        file   TEXT NOT NULL,
        test   TEXT NOT NULL,
        result TEXT NOT NULL,
        job    INTEGER,
        ts     INTEGER NOT NULL
     );
     CREATE INDEX test_runs_test ON test_runs (repo, test);
-- mutants: killed is the JSON list of the tests that failed with the mutation; source tool or agent
     CREATE TABLE mutants (
        id          INTEGER PRIMARY KEY,
        repo        TEXT NOT NULL,
        ref         TEXT NOT NULL,
        file        TEXT NOT NULL,
        line        INTEGER NOT NULL,
        description TEXT NOT NULL,
        killed      TEXT NOT NULL,
        source      TEXT NOT NULL,
        ts          INTEGER NOT NULL
     );
-- tests that the gate lets through without evidence (`workspace checkpoint --accept-test --reason`)
     CREATE TABLE test_accepts (
        repo    TEXT NOT NULL,
        test    TEXT NOT NULL,
        reason  TEXT NOT NULL,
        session TEXT NOT NULL,
        ts      INTEGER NOT NULL
     );
-- threads (design 23, B2): one task; its log is append-only, the tables after it are its projections
     CREATE TABLE threads (
        id      INTEGER PRIMARY KEY,
        title   TEXT NOT NULL,
        dossier TEXT NOT NULL,
        created INTEGER NOT NULL,
        state   TEXT NOT NULL DEFAULT 'open'
     );
     CREATE TABLE thread_log (
        id      INTEGER PRIMARY KEY,
        thread  INTEGER NOT NULL,
        ts      INTEGER NOT NULL,
        member  TEXT NOT NULL,
        kind    TEXT NOT NULL,
        payload TEXT NOT NULL
     );
     CREATE INDEX thread_log_thread ON thread_log (thread, id);
-- cursor: the last log id the member saw (its last wake)
     CREATE TABLE members (
        thread    INTEGER NOT NULL,
        name      TEXT NOT NULL,
        archetype TEXT NOT NULL,
        session   TEXT,
        pace      TEXT NOT NULL,
        cursor    INTEGER NOT NULL DEFAULT 0,
        state     TEXT NOT NULL,
        joined    INTEGER NOT NULL,
        PRIMARY KEY (thread, name)
     );
     CREATE INDEX members_session ON members (session);
-- the outbox: recipient is a member name of the thread or a peer's session id
     CREATE TABLE messages (
        id        INTEGER PRIMARY KEY,
        thread    INTEGER NOT NULL,
        sender    TEXT NOT NULL,
        recipient TEXT NOT NULL,
        kind      TEXT NOT NULL,
        body      TEXT NOT NULL,
        tool      TEXT,
        state     TEXT NOT NULL DEFAULT 'pending',
        created   INTEGER NOT NULL,
        delivered INTEGER,
        acked     INTEGER
     );
-- watch rules and their counts: run = trigger matches since the last expected step, misses = times it fired,
-- done = steps the watching member did for the watched one
     CREATE TABLE watch_rules (
        id      INTEGER PRIMARY KEY,
        thread  INTEGER NOT NULL,
        member  TEXT NOT NULL,
        watched TEXT NOT NULL,
        tools   TEXT NOT NULL,
        path    TEXT NOT NULL,
        expect  TEXT NOT NULL,
        every   INTEGER NOT NULL,
        kind    TEXT NOT NULL,
        created INTEGER NOT NULL,
        run     INTEGER NOT NULL DEFAULT 0,
        misses  INTEGER NOT NULL DEFAULT 0,
        done    INTEGER NOT NULL DEFAULT 0
     );
PRAGMA user_version = 13;
-- two dossiers in @HOME@/repo: alpha (the oldest there, an open workspace) and beta (a closed one, and the live
-- binding of s-live); the rows of a dossier "gone" have no folder
INSERT INTO bindings VALUES ('s-a', 'hook', 'alpha', 100, 200, 300, '@HOME@/repo'),
    ('s-b', 'hook', 'beta', 400, 500, 600, '@HOME@/repo'),
    ('s-live', 'hook', 'beta', 700, @NOW@, NULL, '@HOME@/repo');
INSERT INTO writes VALUES (1, 's-a', 'write', 'alpha/plan.md'), (2, 's-a', 'add', 'alpha/MEMORY/01-x.md'),
    (3, 's-b', 'write', 'beta/plan.md');
INSERT INTO workspaces VALUES ('alpha', '@HOME@/repo', '@HOME@/worktrees/repo/alpha', 'legionctl/alpha', 'main', 100, NULL, ''),
    ('beta', '@HOME@/repo', '@HOME@/worktrees/repo/beta', 'legionctl/beta', 'main', 400, 450, '');
INSERT INTO sessions VALUES ('s-a', 'claude', 'alpha', '@HOME@/worktrees/repo/alpha', 100, NULL, 0, 'host'),
    ('s-b', 'claude', 'beta', '@HOME@/repo', 400, NULL, 0, 'host');
INSERT INTO status_changes VALUES (1, 's-a', 'alpha', 'plan#todo-1', NULL, 'open'),
    (2, 's-a', 'alpha', 'plan#todo-1', 'open', 'done'), (3, 's-b', 'beta', 'plan#todo-2', NULL, 'open'),
    (4, 's-x', 'gone', 'plan#todo-3', NULL, 'open');
INSERT INTO nudges VALUES ('s-a', 'h1', 2, NULL), ('s-b', 'h2', 7, 'info');
-- one annotation of beta's session, one of a session that bound nothing (it goes to the oldest dossier there)
INSERT INTO annotations (repo, key, kind, value, tree, content_hash, session, agent, created) VALUES
    ('@HOME@/repo', 'rust:.:f', 'note', '"by beta"', 't1', 'c1', 's-b', 'claude', 10),
    ('@HOME@/repo', 'rust:.:g', 'note', '"no session"', 't1', 'c2', 's-x', 'codex', 11);
INSERT INTO test_runs VALUES ('@HOME@/repo/.git', 'h', 'tests/e2e.rs', 'a', 'ok', 1, 10),
    ('@HOME@/repo/.git', 'h', 'tests/e2e.rs', 'b', 'ok', 1, 10), ('@HOME@/repo/.git', 'h', 'tests/e2e.rs', 'c', 'FAILED', 1, 10);
INSERT INTO mutants (repo, ref, file, line, description, killed, source, ts) VALUES
    ('@HOME@/repo/.git', 'h', 'src/lib.rs', 2, 'm1', '["a"]', 'agent', 10),
    ('@HOME@/repo/.git', 'h', 'src/lib.rs', 3, 'm2', '[]', 'tool', 11);
INSERT INTO test_accepts VALUES ('@HOME@/repo/.git', 'b', 'why', 's-b', 12);
INSERT INTO threads VALUES (1, 'port it', 'alpha', 100, 'open');
INSERT INTO thread_log VALUES (1, 1, 100, 'main0', 'thread.start', '{}'), (2, 1, 101, 'observer0', 'thread.join', '{}');
INSERT INTO members VALUES (1, 'main0', 'main-agent', 's-a', 'on-trigger', 0, 'active', 100),
    (1, 'observer0', 'thread-observer', 's-b', 'on-stop', 0, 'active', 101);
INSERT INTO messages (thread, sender, recipient, kind, body, tool, state, created) VALUES
    (1, 'observer0', 'main0', 'note', 'hi', NULL, 'pending', 102);
INSERT INTO watch_rules (thread, member, watched, tools, path, expect, every, kind, created) VALUES
    (1, 'observer0', 'main0', 'Edit', 'crates/', 'make lint', 3, 'reminder', 103);
