---
name: workspace-practice
description: How to change code with legionctl - the dossier's workspace (a git worktree), topic workspaces for parallel agents, checkpoint, merge, close, drift, rebase and conflicts. Use before the first code change in a repository, when you start parallel agents on one dossier, and before you commit, merge or close.
user-invocable: true
---

# Workspace practice

1. Change code only in a legionctl workspace. Never edit, stage or commit in the repository's main checkout.
2. If you changed the main checkout, tell the user and move the change to the workspace.
3. Bind a dossier first (skill dossier-practice). A workspace belongs to one dossier.
4. `workspace open [--repo <dir>] [--base <ref>]` makes or reopens the workspace: a worktree at
   `<workspace.root>/<repo>/<dossier>` on branch `legionctl/<dossier>`, with the dossier home linked in as
   `agentWorkspaces` (git ignores it through `info/exclude`).
   The default base is the main checkout's branch.
5. Work in the `path` that `workspace open` returns.
6. A topic workspace is one more worktree of the same dossier for one parallel agent: `workspace open --topic <t>` makes
   `<dossier>--<t>` on `legionctl/<dossier>--<t>`. A reopen continues the same branch.
7. `status`, `checkpoint`, `merge` and `close` act on the workspace that contains the working directory, else on the
   dossier's own workspace, or on the one that `--topic <t>` names.
8. CAUTION: The MCP server's working directory is the main checkout. Give `topic` (or `repo` as the workspace path) to
   each workspace MCP tool, or the tool acts on the dossier's own workspace.
9. A folder outside git gets a folder workspace (an overlay layer, no branch). `--topic` is refused there.
10. Commit with `workspace checkpoint "<message>"`, not raw `git commit`. It stages everything and commits; its test gate
    refuses a new or changed test with no assertion or no kill, and it runs each `gate: checkpoint` check of
    `.legionctl/checks.jsonc` that has no passing run at the tree. Without that file it runs no check (skill
    verify-and-ship, rule 2). Skill verify-and-ship says what the message holds.
11. Write each checkpoint's SHA, scope and tests into its `plan.md` row in the same turn.
12. `workspace status` shows ahead and behind the base, changes, conflicts and drift. If it shows drift, say what
    happened and propose the fix (switch the branch back, or open the workspace again).
13. `workspace merge` rebases the branch onto the base, runs each `gate: merge` check that has no passing run at the
    rebased head (a failure refuses the merge; the branch stays rebased; no file, no check), then fast-forwards the base in the main
    checkout, which must be clean. It is class workspace, so it asks the user. Ask the user before you call it; if a
    lead agent merges, do not merge. An agent cannot merge a branch that changes `.legionctl/`: give the user the
    command that the refusal prints (a terminal with no agent session; a desktop dialog confirms).
14. Merge topics one at a time. Each later merge rebases onto the new base, so the history stays linear.
15. Before you resolve a conflict, read each side's changes against the merge base by entity:
    `legionctl code graph changes --three-way <merge-base> <ours> <theirs>`. It gives base to ours, base to theirs,
    and "changed on both sides": the entities that both sides changed, with the commits of each side. These are the
    real conflicts; other hunks are position only. The text gives one line per file with counts per kind and the code
    entities under it; `--detail` lists the config keys too, `--kind <kind>` filters. A ref without a graph gets one
    with `legionctl code graph refresh <ref>`.
16. Resolve the conflict in the workspace path, run `git rebase --continue`, record the resolution in the dossier, and
    merge again.
17. Plan code in `planning/` (refs/reference-planning.md). Before you work on a feature, read
    `legionctl code plan status --feature <id> [--since <ref>]`; when you change its plan, `legionctl code plan check`
    must pass.
18. `workspace close [--force]` removes the worktree; the dossier stays. It deletes a topic branch that is merged into
    its base (`git branch -d`); an unmerged topic branch and the branch `legionctl/<dossier>` stay. It refuses
    uncommitted changes; ask before `--force`. `--dry-run` shows the workspace it would close and the changes it would
    discard. It refuses a folder in the workspace of another dossier.
19. Do not push. See skill verify-and-ship.

## Refs

- `refs/scenario-one-agent.md` - read for the full flow of one change.
- `refs/scenario-lead-and-topics.md` - read when a lead gives topics to parallel agents.
- `refs/reference-planning.md` - read when you plan a feature or change a file in `planning/`.
