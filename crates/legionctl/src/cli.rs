//! The CLI, generated from the operation registry: `worktree.create` is `legionctl worktree create`. Each input
//! field becomes an argument: a required plain string is positional, a bool is a flag, the rest are `--options`
//! (enum values and defaults come from the schema). Paths are resolved against this process's working directory.

use anyhow::{Context, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};
use legionctl_api::{OPS, OpInfo};
use serde_json::{Map, Value};

/// The schema of one input field, reduced to what an argument needs.
struct Field<'a> {
    key: &'a str,
    help: Option<&'a str>,
    kind: Kind,
    required: bool,
    default: Option<String>,
}

#[derive(PartialEq)]
enum Kind {
    Flag,
    Text,
    Path,
    Integer,
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
            .filter_map(|v| v["const"].as_str().or_else(|| v["enum"][0].as_str()))
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
            } else if let Some(values) = choices(schema) {
                Kind::Choice(values)
            } else {
                match kind {
                    Some("boolean") => Kind::Flag,
                    Some("integer") => Kind::Integer,
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
            }
        })
        .collect()
}

fn argument(field: &Field) -> Arg {
    let mut arg = Arg::new(field.key.to_string());
    if let Some(help) = field.help {
        arg = arg.help(help.to_string());
    }
    if field.kind == Kind::Text && field.required {
        return arg.required(true);
    }
    arg = arg.long(field.key.replace('_', "-"));
    match &field.kind {
        Kind::Flag => return arg.action(ArgAction::SetTrue),
        Kind::Integer => arg = arg.value_parser(clap::value_parser!(i64)),
        Kind::Choice(values) => {
            arg = arg.value_parser(clap::builder::PossibleValuesParser::new(values.clone()));
        }
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
    for field in fields(&schema) {
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
pub fn command(client_only: Vec<Command>) -> Command {
    let root = Command::new("legionctl")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Operate this Legion and the coding agents on it: machine state, load, worktrees, agent setup")
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
    for op in OPS {
        let words: Vec<&'static str> = op.name.split('.').collect();
        tree.insert(&words, op);
    }
    root.subcommands(tree.children.iter().map(|(w, n)| n.command(w)))
        .subcommands(client_only)
}

/// The operation a parsed command line names, and its input.
pub fn operation(matches: &ArgMatches) -> Option<(&'static OpInfo, Value)> {
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
            Kind::Text | Kind::Path | Kind::Choice(_) => at
                .get_one::<String>(field.key)
                .map(|s| Value::from(s.as_str())),
        };
        if let Some(value) = value {
            input.insert(field.key.to_string(), value);
        }
    }
    let mut input = Value::Object(input);
    resolve_paths(op, &mut input).ok()?;
    Some((op, input))
}

/// Make the path fields of `input` absolute against this process's working directory, filling path defaults
/// (such as `.`) that the caller left out.
///
/// # Errors
/// The working directory cannot be read.
pub fn resolve_paths(op: &OpInfo, input: &mut Value) -> Result<()> {
    let schema = (op.input)();
    let Some(map) = input.as_object_mut() else {
        return Ok(());
    };
    for field in fields(&schema).into_iter().filter(|f| f.kind == Kind::Path) {
        let given = map.get(field.key).and_then(Value::as_str).map(String::from);
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
