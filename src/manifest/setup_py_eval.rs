//! Non-executing evaluation of the `setup.py` subset that builds dependency lists.
//!
//! Only literals, names, `+`, string-key subscripts, single-`return` helper
//! functions, and filter-free comprehensions are evaluated. Everything else is
//! [`Value::Unknown`], which callers treat as "could not be read" (#491).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use ruff_python_ast::{Comprehension, Expr, ExprCall, Operator, Stmt, StmtFunctionDef, Suite};

use crate::path_util::rel_to_root;

use super::pep508_util::normalize_distribution_name;
use super::util::path_is_within_root;

const MAX_CALL_DEPTH: usize = 16;
const MAX_COMPREHENSION_ITEMS: usize = 10_000;
/// Largest value kept, in nodes; `x = x + x` doubles without it.
const MAX_VALUE_NODES: usize = 100_000;
/// Total work allowed per file, so fan-out helpers and nested
/// comprehensions end as `Unknown` instead of running away.
const MAX_STEPS: usize = 5_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Value {
    Str(String),
    List(Vec<Self>),
    Dict(Vec<(String, Self)>),
    /// A requirements file that a helper function reads at install time.
    RequirementsFile(PathBuf),
    Unknown,
}

impl Value {
    fn nodes(&self) -> usize {
        match self {
            Self::List(items) => 1 + items.iter().map(Self::nodes).sum::<usize>(),
            Self::Dict(items) => 1 + items.iter().map(|(_, value)| value.nodes()).sum::<usize>(),
            Self::Str(_) | Self::RequirementsFile(_) | Self::Unknown => 1,
        }
    }

    /// Mark a value that code we cannot follow may have changed.
    fn taint(&mut self) {
        match self {
            Self::List(items) => items.push(Self::Unknown),
            _ => *self = Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DependencyItem {
    Requirement(String),
    RequirementsFile(PathBuf),
}

/// Flattened dependency list; `complete` is `false` when part of it was unreadable.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct DependencyItems {
    pub items: Vec<DependencyItem>,
    pub complete: bool,
}

#[derive(Debug)]
pub(super) struct SetupCall {
    pub keywords: Vec<(String, Value)>,
    /// `setup(**kwargs)` / `setup(*args)`: unseen keywords may be passed.
    pub unpacked: bool,
    /// Root-relative requirements file candidates probed but absent.
    pub probed_missing: Vec<String>,
}

impl SetupCall {
    pub fn keyword(&self, name: &str) -> Option<&Value> {
        self.keywords
            .iter()
            .find(|(keyword, _)| keyword == name)
            .map(|(_, value)| value)
    }
}

/// Evaluate `setup.py` statements up to the first `setup(...)` call.
pub(super) fn evaluate_setup_call(root: &Path, stmts: &Suite) -> Option<SetupCall> {
    let mut evaluator = Evaluator {
        root,
        env: HashMap::new(),
        functions: HashMap::new(),
        depth: 0,
        steps: 0,
        probed_missing: Vec::new(),
    };
    let call = evaluator.exec_block(stmts)?;
    let unpacked = call.arguments.keywords.iter().any(|kw| kw.arg.is_none())
        || call
            .arguments
            .args
            .iter()
            .any(|arg| matches!(arg, Expr::Starred(_)));
    let keywords = evaluator.setup_keywords(call);
    Some(SetupCall {
        keywords,
        unpacked,
        probed_missing: evaluator.probed_missing,
    })
}

/// Flatten a dependency-list value into requirement strings and file references.
pub(super) fn dependency_items(value: &Value) -> DependencyItems {
    let mut result = DependencyItems {
        items: Vec::new(),
        complete: true,
    };
    match value {
        // setuptools accepts a newline-separated string for `install_requires`.
        Value::Str(text) => {
            result.items.extend(
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(|line| DependencyItem::Requirement(line.to_owned())),
            );
        },
        _ => flatten_into(value, &mut result),
    }
    result
}

fn flatten_into(value: &Value, result: &mut DependencyItems) {
    match value {
        Value::Str(text) => result.items.push(DependencyItem::Requirement(text.clone())),
        Value::List(items) => {
            for item in items {
                flatten_into(item, result);
            }
        },
        Value::RequirementsFile(path) => result
            .items
            .push(DependencyItem::RequirementsFile(path.clone())),
        Value::Dict(_) | Value::Unknown => result.complete = false,
    }
}

struct Evaluator<'a> {
    root: &'a Path,
    env: HashMap<String, Value>,
    functions: HashMap<String, &'a StmtFunctionDef>,
    depth: usize,
    steps: usize,
    probed_missing: Vec<String>,
}

impl<'a> Evaluator<'a> {
    /// Run statements in order up to the `setup(...)` call. Branches of an
    /// `if` / `try` start from the same state and are merged afterwards.
    fn exec_block(&mut self, stmts: &'a [Stmt]) -> Option<&'a ExprCall> {
        for stmt in stmts {
            match stmt {
                Stmt::Expr(expr) => {
                    if let Expr::Call(call) = &*expr.value {
                        if is_setup(&call.func) {
                            return Some(call);
                        }
                        self.taint_mutation(call);
                    }
                },
                Stmt::Assign(assign) => {
                    let value = self.eval(&assign.value);
                    for target in &assign.targets {
                        self.assign(target, value.clone());
                    }
                },
                Stmt::AnnAssign(assign) => {
                    if let Some(value) = &assign.value {
                        let value = self.eval(value);
                        self.assign(&assign.target, value);
                    }
                },
                Stmt::AugAssign(assign) => {
                    let value = if assign.op == Operator::Add {
                        let sum = add(self.eval(&assign.target), self.eval(&assign.value));
                        self.checked(sum)
                    } else {
                        Value::Unknown
                    };
                    self.assign(&assign.target, value);
                },
                Stmt::FunctionDef(def) => {
                    self.functions.insert(def.name.id.to_string(), def);
                },
                Stmt::If(if_stmt) => {
                    let mut branches = vec![if_stmt.body.as_slice()];
                    branches.extend(if_stmt.elif_else_clauses.iter().map(|c| c.body.as_slice()));
                    let has_else = if_stmt
                        .elif_else_clauses
                        .last()
                        .is_some_and(|clause| clause.test.is_none());
                    if let Some(found) = self.exec_branches(&branches, !has_else) {
                        return Some(found);
                    }
                },
                Stmt::Try(try_stmt) => {
                    let mut branches = vec![try_stmt.body.as_slice()];
                    branches.extend(try_stmt.handlers.iter().map(|handler| {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                        handler.body.as_slice()
                    }));
                    let found = self
                        .exec_branches(&branches, false)
                        .or_else(|| self.exec_block(&try_stmt.orelse))
                        .or_else(|| self.exec_block(&try_stmt.finalbody));
                    if found.is_some() {
                        return found;
                    }
                },
                Stmt::With(with) => {
                    if let Some(found) = self.exec_block(&with.body) {
                        return Some(found);
                    }
                },
                Stmt::For(ruff_python_ast::StmtFor { body, orelse, .. })
                | Stmt::While(ruff_python_ast::StmtWhile { body, orelse, .. }) => {
                    if let Stmt::For(for_stmt) = stmt {
                        self.taint_target(&for_stmt.target);
                    }
                    self.taint_block(body);
                    self.taint_block(orelse);
                },
                _ => {},
            }
        }
        None
    }

    /// Run each branch from the current state and keep the union of what
    /// they assign; `fallthrough` adds the state where no branch ran.
    fn exec_branches(
        &mut self,
        branches: &[&'a [Stmt]],
        fallthrough: bool,
    ) -> Option<&'a ExprCall> {
        let saved = self.env.clone();
        let mut merged: Option<HashMap<String, Value>> = fallthrough.then(|| saved.clone());
        for branch in branches {
            self.env.clone_from(&saved);
            if let Some(found) = self.exec_block(branch) {
                return Some(found);
            }
            let env = std::mem::take(&mut self.env);
            merged = Some(match merged {
                Some(merged) => merge_env(merged, env),
                None => env,
            });
        }
        self.env = merged.unwrap_or(saved);
        None
    }

    /// `reqs.append(...)` and friends change a value we only track by assignment.
    fn taint_mutation(&mut self, call: &ExprCall) {
        if let Expr::Attribute(attribute) = &*call.func
            && let Expr::Name(base) = &*attribute.value
            && let Some(value) = self.env.get_mut(base.id.as_str())
        {
            value.taint();
        }
    }

    fn taint_target(&mut self, target: &Expr) {
        match target {
            Expr::Name(name) => {
                if let Some(value) = self.env.get_mut(name.id.as_str()) {
                    value.taint();
                }
            },
            Expr::Subscript(subscript) => self.taint_target(&subscript.value),
            Expr::Tuple(ruff_python_ast::ExprTuple { elts, .. })
            | Expr::List(ruff_python_ast::ExprList { elts, .. }) => {
                for elt in elts {
                    self.taint_target(elt);
                }
            },
            _ => {},
        }
    }

    /// Loop bodies are not run; everything they may change becomes partial.
    fn taint_block(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match stmt {
                Stmt::Expr(expr) => {
                    if let Expr::Call(call) = &*expr.value {
                        self.taint_mutation(call);
                    }
                },
                Stmt::Assign(assign) => {
                    for target in &assign.targets {
                        self.taint_target(target);
                    }
                },
                Stmt::AnnAssign(ruff_python_ast::StmtAnnAssign { target, .. })
                | Stmt::AugAssign(ruff_python_ast::StmtAugAssign { target, .. }) => {
                    self.taint_target(target);
                },
                Stmt::For(for_stmt) => {
                    self.taint_target(&for_stmt.target);
                    self.taint_block(&for_stmt.body);
                    self.taint_block(&for_stmt.orelse);
                },
                Stmt::While(while_stmt) => {
                    self.taint_block(&while_stmt.body);
                    self.taint_block(&while_stmt.orelse);
                },
                Stmt::If(if_stmt) => {
                    self.taint_block(&if_stmt.body);
                    for clause in &if_stmt.elif_else_clauses {
                        self.taint_block(&clause.body);
                    }
                },
                Stmt::Try(try_stmt) => {
                    self.taint_block(&try_stmt.body);
                    for handler in &try_stmt.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                        self.taint_block(&handler.body);
                    }
                    self.taint_block(&try_stmt.orelse);
                    self.taint_block(&try_stmt.finalbody);
                },
                Stmt::With(with) => self.taint_block(&with.body),
                _ => {},
            }
        }
    }

    /// Charge `cost` against the step budget; `false` once it is spent.
    fn spend(&mut self, cost: usize) -> bool {
        self.steps = self.steps.saturating_add(cost);
        self.steps <= MAX_STEPS
    }

    fn env_nodes(&self) -> usize {
        self.env.values().map(Value::nodes).sum()
    }

    fn setup_keywords(&mut self, call: &ExprCall) -> Vec<(String, Value)> {
        call.arguments
            .keywords
            .iter()
            .filter_map(|keyword| {
                let name = keyword.arg.as_ref()?.as_str();
                matches!(
                    name,
                    "name" | "version" | "install_requires" | "extras_require"
                )
                .then(|| (name.to_owned(), self.eval(&keyword.value)))
            })
            .collect()
    }

    fn assign(&mut self, target: &Expr, value: Value) {
        match target {
            Expr::Name(name) => {
                self.env.insert(name.id.to_string(), value);
            },
            Expr::Subscript(subscript) => {
                let (Expr::Name(base), Value::Str(key)) =
                    (&*subscript.value, self.eval(&subscript.slice))
                else {
                    return;
                };
                if let Some(Value::Dict(items)) = self.env.get_mut(base.id.as_str()) {
                    insert_entry(items, key, value);
                }
            },
            Expr::Tuple(ruff_python_ast::ExprTuple { elts, .. })
            | Expr::List(ruff_python_ast::ExprList { elts, .. }) => {
                let values = match value {
                    Value::List(values) if values.len() == elts.len() => values,
                    _ => vec![Value::Unknown; elts.len()],
                };
                for (target, value) in elts.iter().zip(values) {
                    self.assign(target, value);
                }
            },
            _ => {},
        }
    }

    fn eval(&mut self, expr: &Expr) -> Value {
        if !self.spend(1) {
            return Value::Unknown;
        }
        let value = self.eval_uncharged(expr);
        self.checked(value)
    }

    /// Drop values over the size cap and charge the rest by size.
    fn checked(&mut self, value: Value) -> Value {
        let nodes = value.nodes();
        if nodes > MAX_VALUE_NODES || !self.spend(nodes) {
            return Value::Unknown;
        }
        value
    }

    fn eval_uncharged(&mut self, expr: &Expr) -> Value {
        match expr {
            Expr::StringLiteral(literal) => Value::Str(literal.value.to_str().to_owned()),
            Expr::List(ruff_python_ast::ExprList { elts, .. })
            | Expr::Tuple(ruff_python_ast::ExprTuple { elts, .. })
            | Expr::Set(ruff_python_ast::ExprSet { elts, .. }) => {
                Value::List(self.eval_elements(elts))
            },
            Expr::Dict(dict) => {
                let mut items: Vec<(String, Value)> = Vec::new();
                for item in &dict.items {
                    match item.key.as_ref().map(|key| self.eval(key)) {
                        Some(Value::Str(key)) => {
                            let value = self.eval(&item.value);
                            insert_entry(&mut items, key, value);
                        },
                        None => {
                            if let Value::Dict(spread) = self.eval(&item.value) {
                                for (key, value) in spread {
                                    insert_entry(&mut items, key, value);
                                }
                            }
                        },
                        Some(_) => {},
                    }
                }
                Value::Dict(items)
            },
            Expr::Name(name) => self
                .env
                .get(name.id.as_str())
                .cloned()
                .unwrap_or(Value::Unknown),
            Expr::BinOp(binop) if binop.op == Operator::Add => {
                add(self.eval(&binop.left), self.eval(&binop.right))
            },
            Expr::Subscript(subscript) => {
                let base = self.eval(&subscript.value);
                let Value::Str(key) = self.eval(&subscript.slice) else {
                    return Value::Unknown;
                };
                match base {
                    Value::Dict(items) => items
                        .into_iter()
                        .find(|(existing, _)| *existing == key)
                        .map_or(Value::Unknown, |(_, value)| value),
                    // Only a table this file assigned, not `os.environ[...]`.
                    Value::Unknown if matches!(&*subscript.value, Expr::Name(name) if self.env.contains_key(name.id.as_str())) => {
                        self.requirement_table_lookup(&key)
                    },
                    _ => Value::Unknown,
                }
            },
            Expr::Call(call) => self.eval_call(call),
            Expr::ListComp(comp) => self.eval_list_comprehension(&comp.elt, &comp.generators),
            Expr::SetComp(comp) => self.eval_list_comprehension(&comp.elt, &comp.generators),
            Expr::Generator(comp) => self.eval_list_comprehension(&comp.elt, &comp.generators),
            Expr::DictComp(comp) => {
                let Some(key) = comp.key.as_deref() else {
                    return Value::Unknown;
                };
                let Some(pairs) = self.comprehend(&comp.generators, |evaluator| {
                    (evaluator.eval(key), evaluator.eval(&comp.value))
                }) else {
                    return Value::Unknown;
                };
                let mut items = Vec::with_capacity(pairs.len());
                for (key, value) in pairs {
                    let Value::Str(key) = key else {
                        return Value::Unknown;
                    };
                    insert_entry(&mut items, key, value);
                }
                Value::Dict(items)
            },
            _ => Value::Unknown,
        }
    }

    fn eval_elements(&mut self, elts: &[Expr]) -> Vec<Value> {
        let mut values = Vec::with_capacity(elts.len());
        for elt in elts {
            if let Expr::Starred(starred) = elt {
                match self.eval(&starred.value) {
                    Value::List(spread) => values.extend(spread),
                    _ => values.push(Value::Unknown),
                }
            } else {
                values.push(self.eval(elt));
            }
        }
        values
    }

    fn eval_list_comprehension(&mut self, elt: &Expr, generators: &[Comprehension]) -> Value {
        self.comprehend(generators, |evaluator| evaluator.eval(elt))
            .map_or(Value::Unknown, Value::List)
    }

    /// Evaluate `produce` once per binding of filter-free generators.
    fn comprehend<T>(
        &mut self,
        generators: &[Comprehension],
        mut produce: impl FnMut(&mut Self) -> T,
    ) -> Option<Vec<T>> {
        let saved = self.env.clone();
        let scope_cost = self.env_nodes().max(1);
        let mut scopes = vec![saved.clone()];
        for generator in generators {
            if !generator.ifs.is_empty() || generator.is_async {
                self.env = saved;
                return None;
            }
            let mut next = Vec::new();
            for scope in scopes {
                self.env = scope;
                let Value::List(items) = self.eval(&generator.iter) else {
                    self.env = saved;
                    return None;
                };
                for item in items {
                    if next.len() >= MAX_COMPREHENSION_ITEMS || !self.spend(scope_cost) {
                        self.env = saved;
                        return None;
                    }
                    self.assign(&generator.target, item);
                    next.push(self.env.clone());
                }
            }
            scopes = next;
        }
        let mut produced = Vec::with_capacity(scopes.len());
        for scope in scopes {
            self.env = scope;
            produced.push(produce(self));
        }
        self.env = saved;
        Some(produced)
    }

    fn eval_call(&mut self, call: &ExprCall) -> Value {
        let args = self.eval_elements(&call.arguments.args);
        let Expr::Name(func) = &*call.func else {
            return self.requirements_file(&args);
        };
        let name = func.id.as_str();
        if matches!(name, "list" | "tuple" | "sorted" | "set")
            && call.arguments.keywords.is_empty()
            && let [arg] = args.as_slice()
            && matches!(arg, Value::List(_) | Value::RequirementsFile(_))
        {
            return arg.clone();
        }
        let value = match self.functions.get(name).copied() {
            Some(def) if call.arguments.keywords.is_empty() => {
                self.call_function(def, args.clone())
            },
            _ => Value::Unknown,
        };
        if value == Value::Unknown {
            self.requirements_file(&args)
        } else {
            value
        }
    }

    /// Inline a helper whose body is an optional docstring plus one `return`.
    fn call_function(&mut self, def: &'a StmtFunctionDef, args: Vec<Value>) -> Value {
        if self.depth >= MAX_CALL_DEPTH || !self.spend(self.env_nodes()) {
            return Value::Unknown;
        }
        let body = match def.body.as_slice() {
            [Stmt::Expr(doc), rest @ ..] if matches!(&*doc.value, Expr::StringLiteral(_)) => rest,
            body => body,
        };
        let [Stmt::Return(ret)] = body else {
            return Value::Unknown;
        };
        let Some(returned) = ret.value.as_deref() else {
            return Value::Unknown;
        };

        let parameters = &def.parameters;
        let mut positional = args.into_iter();
        let mut locals: Vec<(String, Value)> = parameters
            .posonlyargs
            .iter()
            .chain(&parameters.args)
            .map(|param| {
                let value = positional.next().unwrap_or(Value::Unknown);
                (param.parameter.name.id.to_string(), value)
            })
            .collect();
        if let Some(vararg) = &parameters.vararg {
            locals.push((
                vararg.name.id.to_string(),
                Value::List(positional.collect()),
            ));
        }

        let saved = self.env.clone();
        self.env.extend(locals);
        self.depth += 1;
        let value = self.eval(returned);
        self.depth -= 1;
        self.env = saved;
        value
    }

    /// `helper("default.txt")` / `helper("extras", "redis.txt")`: an
    /// unevaluable call whose string arguments name an existing requirements
    /// file at the root or under `requirements/`.
    fn requirements_file(&mut self, args: &[Value]) -> Value {
        let mut relative = PathBuf::new();
        for arg in args {
            let Value::Str(part) = arg else {
                return Value::Unknown;
            };
            relative.push(part);
        }
        let is_requirements = relative
            .extension()
            .is_some_and(|ext| ext == "txt" || ext == "in");
        let stays_inside = relative
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
        if !is_requirements || !stays_inside {
            return Value::Unknown;
        }
        for base in [self.root.to_path_buf(), self.root.join("requirements")] {
            let candidate = base.join(&relative);
            if candidate.is_file() {
                if path_is_within_root(self.root, &candidate) {
                    return Value::RequirementsFile(candidate);
                }
            } else {
                self.probed_missing.push(rel_to_root(self.root, &candidate));
            }
        }
        Value::Unknown
    }

    /// `deps["tokenizers"]` on a table built at install time (transformers):
    /// find a known requirement string whose name matches the key. The bare
    /// key itself (e.g. a `deps_list("torch")` argument) loses to a fuller one.
    fn requirement_table_lookup(&self, key: &str) -> Value {
        let wanted = normalize_distribution_name(key);
        let mut found: Vec<&String> = Vec::new();
        for value in self.env.values() {
            let Value::List(items) = value else {
                continue;
            };
            for item in items {
                if let Value::Str(raw) = item
                    && normalize_distribution_name(requirement_table_key(raw)) == wanted
                    && !found.contains(&raw)
                {
                    found.push(raw);
                }
            }
        }
        if found.len() > 1 {
            found.retain(|raw| raw.as_str() != key);
        }
        match found.as_slice() {
            [raw] => Value::Str((*raw).clone()),
            _ => Value::Unknown,
        }
    }
}

/// Requirement text before its version or marker, extras kept.
fn requirement_table_key(raw: &str) -> &str {
    raw.split(['!', '=', '<', '>', '~', ' ', ';', '@'])
        .next()
        .unwrap_or(raw)
        .trim()
}

/// `dict[key] = value`: a repeated key keeps its first position and the last value.
fn insert_entry(items: &mut Vec<(String, Value)>, key: String, value: Value) {
    match items.iter_mut().find(|(existing, _)| *existing == key) {
        Some((_, slot)) => *slot = value,
        None => items.push((key, value)),
    }
}

fn add(left: Value, right: Value) -> Value {
    match (left, right) {
        (Value::Str(left), Value::Str(right)) => Value::Str(left + &right),
        (Value::List(mut left), Value::List(right)) => {
            left.extend(right);
            Value::List(left)
        },
        (Value::List(mut items), other) | (other, Value::List(mut items))
            if !matches!(other, Value::Str(_)) =>
        {
            items.push(other);
            Value::List(items)
        },
        _ => Value::Unknown,
    }
}

/// State after one of two branches ran: lists and dicts keep every entry
/// either branch gave, so an optional dependency still counts as declared.
fn merge_env(
    mut left: HashMap<String, Value>,
    right: HashMap<String, Value>,
) -> HashMap<String, Value> {
    for (name, value) in right {
        let merged = match left.remove(&name) {
            Some(existing) => merge(existing, value),
            None => value,
        };
        left.insert(name, merged);
    }
    left
}

fn merge(left: Value, right: Value) -> Value {
    match (left, right) {
        (left, right) if left == right => left,
        (Value::List(mut left), Value::List(right)) => {
            for item in right {
                if !left.contains(&item) {
                    left.push(item);
                }
            }
            Value::List(left)
        },
        (Value::Dict(mut left), Value::Dict(right)) => {
            for (key, value) in right {
                match left.iter().position(|(existing, _)| *existing == key) {
                    Some(index) => {
                        let (_, existing) = left.remove(index);
                        left.insert(index, (key, merge(existing, value)));
                    },
                    None => left.push((key, value)),
                }
            }
            Value::Dict(left)
        },
        _ => Value::Unknown,
    }
}

fn is_setup(func: &Expr) -> bool {
    match func {
        Expr::Name(name) => name.id.as_str() == "setup",
        Expr::Attribute(attribute) => attribute.attr.as_str() == "setup",
        _ => false,
    }
}
