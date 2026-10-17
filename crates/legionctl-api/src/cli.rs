//! The CLI, generated from the operation registry: `workspace.open` is `legionctl workspace open`. Each input
//! field becomes an argument: a required plain string is positional, a bool is a flag, the rest are `--options`
//! (enum values and defaults come from the schema). Paths are resolved against this process's working directory.

use std::io::IsTerminal;

use crate::{OPS, OpInfo};
use anyhow::{Context, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value};

/// The schema of one input field, reduced to what an argument needs.
struct Field<'a> {
    key: &'a str,
    help: Option<&'a str>,
    kind: Kind,
    required: bool,
    default: Option<String>,
    /// `x-env`: the client's environment variable that gives a path field when the caller does not.
    env: Option<&'a str>,
    /// `x-from-env`: a secret that the CLI takes only as `--<key>-env <VAR>`, the name of its environment variable.
    from_env: bool,
}

#[derive(PartialEq)]
enum Kind {
    /// The agent session: filled from the environment when not given.
    Session,
    /// Text the CLI reads from stdin when not given.
    Stdin,
    Flag,
    Text,
    Number,
    Path,
    Integer,
    /// A list of strings, or of integers: the last positional arguments; a command line (`x-argv`) takes the rest
    /// of the line, words with hyphens too; `x-option`: a `--name` that repeats.
    List {
        integers: bool,
        argv: bool,
        option: bool,
    },
    Choice(Vec<String>),
}

/// `Option<T>` is `anyOf [T, null]` or `type: [T, "null"]`; return T's schema and its type name.
fn base(prop: &Value) -> (&Value, Option<&str>) {
    let schema = prop["anyOf"]
        .as_array()
        .and_then(|any| any.iter().find(|s| s["type"] != "null"))
        .unwrap_or(prop);
    let kind = match &schema["type"] {
        Value::String(t) => Some(t.as_str()),
        Value::Array(types) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null"),
        _ => None,
    };
    (schema, kind)
}

fn choices(schema: &Value) -> Option<Vec<String>> {
    let values: Vec<String> = if let Some(values) = schema["enum"].as_array() {
        values
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect()
    } else {
        schema["oneOf"]
            .as_array()?
            .iter()
            // a variant group without docs is one entry with an enum of all its values
            .flat_map(|v| match v["const"].as_str() {
                Some(c) => vec![c],
                None => v["enum"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect(),
            })
            .map(String::from)
            .collect()
    };
    (!values.is_empty()).then_some(values)
}

fn fields(input: &Value) -> Vec<Field<'_>> {
    let required: Vec<&str> = input["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    input["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, prop)| {
            let (schema, kind) = base(prop);
            let kind = if schema["format"] == "path" {
                Kind::Path
            } else if schema["format"] == "session" {
                Kind::Session
            } else if schema["format"] == "stdin" {
                Kind::Stdin
            } else if let Some(values) = choices(schema) {
                Kind::Choice(values)
            } else {
                match kind {
                    Some("boolean") => Kind::Flag,
                    Some("integer") => Kind::Integer,
                    Some("number") => Kind::Number,
                    Some("array") => Kind::List {
                        integers: schema["items"]["type"] == "integer",
                        argv: prop["x-argv"] == true,
                        option: prop["x-option"] == true,
                    },
                    _ => Kind::Text,
                }
            };
            Field {
                key,
                help: prop["description"]
                    .as_str()
                    .or(schema["description"].as_str()),
                kind,
                required: required.contains(&key.as_str()),
                default: match &prop["default"] {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                },
                env: prop["x-env"].as_str(),
                from_env: prop["x-from-env"] == true,
            }
        })
        .collect()
}

fn argument(field: &Field) -> Arg {
    let mut arg = Arg::new(field.key.to_string());
    if let Some(help) = field.help {
        arg = arg.help(help.to_string());
    }
    // a required value with no default is a positional, in field order: `gpu primary nvidia`, `bench compare 1 2`
    if field.required && field.default.is_none() {
        match &field.kind {
            Kind::Integer => return arg.required(true).value_parser(clap::value_parser!(i64)),
            Kind::Number => return arg.required(true).value_parser(clap::value_parser!(f64)),
            Kind::Choice(values) => {
                return arg
                    .required(true)
                    .value_parser(clap::builder::PossibleValuesParser::new(values.clone()));
            }
            Kind::Text | Kind::Path => return arg.required(true),
            Kind::Flag | Kind::List { .. } | Kind::Session | Kind::Stdin => {}
        }
    }
    argument_named(field, arg)
}

/// A field as `--name` (or the last positional list, or a field the client fills).
fn argument_named(field: &Field, arg: Arg) -> Arg {
    if let Kind::List {
        integers,
        argv,
        option,
    } = field.kind
    {
        // options may follow a plain list; a command line keeps its own options (`gates check push --force`)
        let mut list = if option {
            arg.long(field.key.replace('_', "-"))
                .num_args(1)
                .action(ArgAction::Append)
        } else {
            arg.num_args(0..).action(ArgAction::Append)
        };
        if argv {
            list = list.trailing_var_arg(true).allow_hyphen_values(true);
        }
        return if integers {
            list.value_parser(clap::value_parser!(i64))
        } else {
            list
        };
    }
    if field.from_env {
        return arg
            .long(format!("{}-env", field.key.replace('_', "-")))
            .value_name("VAR");
    }
    let mut arg = arg.long(field.key.replace('_', "-"));
    match &field.kind {
        Kind::Flag => return arg.action(ArgAction::SetTrue),
        Kind::Integer => arg = arg.value_parser(clap::value_parser!(i64)),
        Kind::Number => arg = arg.value_parser(clap::value_parser!(f64)),
        Kind::Choice(values) => {
            arg = arg.value_parser(clap::builder::PossibleValuesParser::new(values.clone()));
        }
        Kind::Session | Kind::List { .. } => return arg,
        // free text may start with '-': `--content "- a ledger line"`
        Kind::Stdin => return arg.allow_hyphen_values(true),
        Kind::Text | Kind::Path => {}
    }
    match &field.default {
        Some(default) => arg.default_value(default.clone()),
        None => arg.required(field.required),
    }
}

fn leaf(op: &OpInfo, word: &str) -> Command {
    let schema = (op.input)();
    let mut command = Command::new(word.to_string())
        .about(op.summary())
        .long_about(op.text());
    let mut fields = fields(&schema);
    // positionals in field order, and a trailing list after all of them
    fields.sort_by_key(|f| match f.kind {
        Kind::List { option: false, .. } => 2,
        _ if f.required && f.default.is_none() => 0,
        _ => 1,
    });
    for field in fields {
        command = command.arg(argument(&field));
    }
    command
}

/// The words of the dotted operation names as a tree, in registry order.
#[derive(Default)]
struct Node {
    op: Option<&'static OpInfo>,
    children: Vec<(&'static str, Node)>,
}

impl Node {
    fn insert(&mut self, words: &[&'static str], op: &'static OpInfo) {
        let Some((first, rest)) = words.split_first() else {
            self.op = Some(op);
            return;
        };
        let i = self
            .children
            .iter()
            .position(|(w, _)| w == first)
            .unwrap_or_else(|| {
                self.children.push((first, Node::default()));
                self.children.len() - 1
            });
        self.children[i].1.insert(rest, op);
    }

    fn command(&self, word: &str) -> Command {
        match self.op {
            Some(op) => leaf(op, word),
            None => Command::new(word.to_string())
                .about(
                    self.children
                        .iter()
                        .map(|(w, _)| *w)
                        .collect::<Vec<_>>()
                        .join(", "),
                )
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommands(self.children.iter().map(|(w, n)| n.command(w))),
        }
    }
}

/// Every operation, plus the commands only a local process can do (`client_only`).
#[must_use]
pub fn command(client_only: Vec<Command>) -> Command {
    let root = Command::new("legionctl")
        .version(crate::version())
        .about("Operate this Legion and the coding agents on it: machine state, load, dossiers and workspaces, agent setup")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(
            Arg::new("json")
                .long("json")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Print the operation's JSON output"),
        );
    let mut tree = Node::default();
    for op in OPS.iter().filter(|op| !op.internal()) {
        let words: Vec<&'static str> = op.name.split('.').collect();
        tree.insert(&words, op);
    }
    root.subcommands(tree.children.iter().map(|(w, n)| n.command(w)))
        .subcommands(client_only)
}

/// The operation a parsed command line names, and its input; `stdin` as in [`fill_client_fields`].
///
/// # Errors
/// A client-filled field cannot be filled (see [`fill_client_fields`]).
pub fn operation(matches: &ArgMatches, stdin: bool) -> Result<Option<(&'static OpInfo, Value)>> {
    let Some((op, mut input)) = parse(matches) else {
        return Ok(None);
    };
    fill_client_fields(op, &mut input, stdin)?;
    Ok(Some((op, input)))
}

/// The operation a parsed command line names, and the input that the line gives (no field of the calling process).
#[must_use]
pub fn parse(matches: &ArgMatches) -> Option<(&'static OpInfo, Value)> {
    let mut words = Vec::new();
    let mut at = matches;
    while let Some((word, sub)) = at.subcommand() {
        words.push(word);
        at = sub;
    }
    let name = words.join(".");
    let op = OPS.iter().find(|op| op.name == name)?;
    let schema = (op.input)();
    let mut input = Map::new();
    for field in fields(&schema) {
        let value = match field.kind {
            Kind::Flag => Some(Value::Bool(at.get_flag(field.key))),
            Kind::Integer => at.get_one::<i64>(field.key).map(|n| Value::from(*n)),
            Kind::Number => at.get_one::<f64>(field.key).map(|n| Value::from(*n)),
            Kind::List { integers: true, .. } => Some(Value::from(
                at.get_many::<i64>(field.key)
                    .into_iter()
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>(),
            )),
            Kind::List {
                integers: false, ..
            } => Some(Value::from(
                at.get_many::<String>(field.key)
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>(),
            )),
            Kind::Text | Kind::Path | Kind::Choice(_) | Kind::Session | Kind::Stdin => at
                .get_one::<String>(field.key)
                .map(|s| Value::from(s.as_str())),
        };
        if let Some(value) = value {
            input.insert(field.key.to_string(), value);
        }
    }
    Some((op, Value::Object(input)))
}

/// The agent session of this process: `LEGIONCTL_SESSION`, else Claude Code's `CLAUDE_CODE_SESSION_ID`.
#[must_use]
pub fn session_from_env() -> Option<String> {
    ["LEGIONCTL_SESSION", "CLAUDE_CODE_SESSION_ID"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
}

/// Fill what only the calling process knows: path fields become absolute against its working directory (with path
/// defaults such as `.`, and a path field with `x-env` from that environment variable when not given), session fields
/// come from its environment, and with `stdin` a missing text field is read
/// from stdin (always when the field is required; when optional, only from a pipe).
///
/// # Errors
/// The working directory or stdin cannot be read.
pub fn fill_client_fields(op: &OpInfo, input: &mut Value, stdin: bool) -> Result<()> {
    let schema = (op.input)();
    let Some(map) = input.as_object_mut() else {
        return Ok(());
    };
    for field in fields(&schema).into_iter().filter(|f| f.from_env) {
        let Some(var) = map.get(field.key).and_then(Value::as_str) else {
            continue;
        };
        // only a CLI reads its own environment: through MCP, the backend's keyring entry gives the secret
        anyhow::ensure!(stdin, "--{}-env is for the CLI only", field.key);
        let secret = std::env::var(var)
            .ok()
            .filter(|v| !v.is_empty())
            .with_context(|| format!("the environment variable {var} is not set or empty"))?;
        map.insert(field.key.to_string(), Value::from(secret));
    }
    for field in fields(&schema) {
        if map.contains_key(field.key) && field.kind != Kind::Path {
            continue;
        }
        match field.kind {
            Kind::Session => {
                if let Some(id) = session_from_env() {
                    map.insert(field.key.to_string(), Value::from(id));
                }
            }
            Kind::Stdin if stdin && (field.required || !std::io::stdin().is_terminal()) => {
                let text = std::io::read_to_string(std::io::stdin()).context("reading stdin")?;
                map.insert(field.key.to_string(), Value::from(text));
            }
            _ => {}
        }
    }
    for field in fields(&schema).into_iter().filter(|f| f.kind == Kind::Path) {
        let given = map
            .get(field.key)
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| std::env::var(field.env?).ok().filter(|v| !v.is_empty()));
        if let Some(path) = given.or(field.default) {
            let absolute =
                std::path::absolute(&path).with_context(|| format!("resolving {path}"))?;
            map.insert(
                field.key.to_string(),
                Value::from(absolute.to_string_lossy().as_ref()),
            );
        }
    }
    Ok(())
}

/// Fill a line of the user's chat bar (the app): the user is no agent session, so no session field comes
/// from the process's environment (a chat bar started from an agent's terminal still acts as the user), and path
/// fields (with their defaults such as `.`) are against `dir`, the chat bar's folder, not the process's.
pub fn fill_user_fields(op: &OpInfo, input: &mut Value, dir: &std::path::Path) {
    let schema = (op.input)();
    let Some(map) = input.as_object_mut() else {
        return;
    };
    for field in fields(&schema).into_iter().filter(|f| f.kind == Kind::Path) {
        let given = map.get(field.key).and_then(Value::as_str).map(String::from);
        if let Some(path) = given.or(field.default) {
            map.insert(
                field.key.to_string(),
                Value::from(dir.join(path).to_string_lossy().as_ref()),
            );
        }
    }
}
