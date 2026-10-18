//! Gates on commands inside a session sandbox (note 14, layer 2): `/usr/bin/git` there is legionctl, which asks the
//! operator before it runs the real git. A rule is a subcommand and the flags that make it unsafe: `push`,
//! `reset --hard`, `clean -f` (a short flag also matches inside a bundle such as `-fd`). Aliases are refused, because
//! an alias could name a denied command.

/// Git options that come before the subcommand and take a value in the next argument.
const VALUE_OPTIONS: [&str; 6] = [
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
];

/// The subcommand of a git command line and the arguments after it.
fn split(args: &[String]) -> Option<(&str, &[String])> {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        if !arg.starts_with('-') {
            return Some((arg.as_str(), &args[i + 1..]));
        }
        i += if VALUE_OPTIONS.contains(&arg.as_str()) {
            2
        } else {
            1
        };
    }
    None
}

/// The subcommand of the git command line `args`.
#[must_use]
pub fn subcommand(args: &[String]) -> Option<&str> {
    split(args).map(|(sub, _)| sub)
}

/// Whether `arg` is the flag `flag`: the same token, `--flag=value`, or a short flag inside a bundle (`-fd` has `-f`).
fn has_flag(arg: &str, flag: &str) -> bool {
    if arg == flag
        || arg
            .strip_prefix(flag)
            .is_some_and(|rest| rest.starts_with('='))
    {
        return true;
    }
    match (flag.strip_prefix('-'), arg.strip_prefix('-')) {
        (Some(short), Some(bundle)) if short.len() == 1 && !bundle.starts_with('-') => {
            bundle.contains(short)
        }
        _ => false,
    }
}

/// Why `git <args>` is refused under `deny`, or `None` when it may run. `is_alias` says whether a subcommand is a
/// git alias in this repository.
pub fn git_refusal(
    args: &[String],
    deny: &[String],
    is_alias: impl Fn(&str) -> anyhow::Result<bool>,
) -> Option<String> {
    let (sub, rest) = split(args)?;
    match is_alias(sub) {
        Ok(true) => {
            return Some(format!(
                "git aliases are refused in a legionctl sandbox ('{sub}'); use the command itself"
            ));
        }
        Ok(false) => {}
        // the gate cannot see what the command runs: it refuses
        Err(e) => return Some(format!("legionctl gate: cannot check git aliases: {e:#}")),
    }
    deny.iter().find_map(|rule| {
        let mut words = rule.split_whitespace();
        let rule_sub = words.next()?;
        let flags: Vec<&str> = words.collect();
        let matches = rule_sub == sub && flags.iter().all(|f| rest.iter().any(|a| has_flag(a, f)));
        matches.then(|| {
            format!(
                "legionctl gate: 'git {rule}' is refused in this session (config gates.git.deny). Merge with \
                 workspace_merge, and leave pushes and history changes to the user."
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_git_is_refused_through_global_options_bundles_and_aliases() {
        let deny: Vec<String> = [
            "push",
            "reset --hard",
            "clean -f",
            "branch -D",
            "gc --prune",
        ]
        .map(String::from)
        .to_vec();
        let refused = |line: &str| {
            let args: Vec<String> = line.split_whitespace().map(String::from).collect();
            git_refusal(&args, &deny, |sub| Ok(sub == "p")).is_some()
        };
        // global options before the subcommand do not hide it
        assert!(refused("-C /repo -c user.name=x push origin main"));
        assert!(refused("--no-pager reset --hard HEAD~1"));
        assert!(refused("clean -fdx"), "a short flag inside a bundle");
        assert!(refused("gc --prune=now"), "--flag=value");
        assert!(refused("p origin"), "an alias could name push");
        // the same subcommands without the unsafe flag, and local work, run
        assert!(!refused("reset --soft HEAD~1"));
        assert!(!refused("clean -n"));
        assert!(!refused("branch -d done"), "-d is not -D");
        assert!(
            !refused("-C /repo commit -m push"),
            "an argument that looks like a rule is not a subcommand"
        );
        assert!(!refused("status"));
    }
}
