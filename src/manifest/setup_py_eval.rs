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
                Stmt::Delete(delete) => self.delete(&delete.targets),
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
        let Expr::Attribute(attribute) = &*call.func else {
            return;
        };
        if let Some(value) = self.place_mut(&attribute.value) {
            value.taint();
        } else {
            self.taint_target(&attribute.value);
        }
    }

    fn delete(&mut self, targets: &[Expr]) {
        for target in targets {
            match target {
                Expr::Name(name) => {
                    self.env.remove(name.id.as_str());
                },
                Expr::Subscript(subscript) => {
                    if let Value::Str(key) = self.eval(&subscript.slice)
                        && let Some(Value::Dict(items)) = self.place_mut(&subscript.value)
                    {
                        items.retain(|(existing, _)| *existing != key);
                    } else {
                        self.taint_target(&subscript.value);
                    }
                },
                Expr::Tuple(ruff_python_ast::ExprTuple { elts, .. })
                | Expr::List(ruff_python_ast::ExprList { elts, .. }) => self.delete(elts),
                _ => {},
            }
        }
    }

    /// The stored value a name or string-key subscript chain refers to.
    fn place_mut(&mut self, expr: &Expr) -> Option<&mut Value> {
        match expr {
            Expr::Name(name) => self.env.get_mut(name.id.as_str()),
            Expr::Subscript(subscript) => {
                let Value::Str(key) = self.eval(&subscript.slice) else {
                    return None;
                };
                match self.place_mut(&subscript.value)? {
                    Value::Dict(items) => items
                        .iter_mut()
                        .find(|(existing, _)| *existing == key)
                        .map(|(_, value)| value),
                    _ => None,
                }
            },
            _ => None,
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
                Stmt::Delete(delete) => {
                    for target in &delete.targets {
                        self.taint_target(target);
                    }
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

#[cfg(test)]
mod props {
    //! Generated `setup.py` programs checked against a small model of the
    //! Python semantics of the evaluated subset (cross-checked offline with
    //! `CPython`). Lists are only rebound (`v = v + ...`), never mutated in
    //! place, and dicts are only copied (`{**d}`): the evaluator has value
    //! semantics, so `b = a; a.append(x)` (Python aliasing) is out of scope.

    use std::collections::HashMap;
    use std::fmt::Write as _;
    use std::path::Path;

    use proptest::prelude::*;

    use super::*;
    use crate::manifest::literals::parse_module;

    const STRINGS: [&str; 5] = ["a", "b", "req", "c1", "dev"];
    const KEYS: [&str; 3] = ["a", "b", "c"];

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Py {
        Str(String),
        List(Vec<Self>),
        Dict(Vec<(String, Self)>),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Str,
        List,
        Dict,
    }

    #[derive(Debug, Clone)]
    enum Ex {
        Lit(&'static str),
        Var(String),
        Add(Box<Self>, Box<Self>),
        /// `[*spread, items...]`
        List(Option<Box<Self>>, Vec<Self>),
        /// `[x + suffix for x in iter]`
        Comp(Box<Self>, &'static str),
        /// `list(inner)`
        Copy(Box<Self>),
        /// `{**base, key: value, ...}`
        Dict(Option<Box<Self>>, Vec<(&'static str, Self)>),
        /// `{k: [k] for k in iter}`
        DictComp(Box<Self>),
        Sub(Box<Self>, &'static str),
        Call(String, Vec<Self>),
    }

    #[derive(Debug, Clone)]
    enum St {
        Assign(String, Ex),
        AugAdd(String, Ex),
        SetItem(String, &'static str, Ex),
        Del(String, &'static str),
        Def(String, Vec<String>, Ex),
    }

    /// Choices drawn from proptest bytes, so shrinking the bytes shrinks the program.
    struct Tape<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl Tape<'_> {
        fn pick(&mut self, n: usize) -> usize {
            let byte = self.bytes.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            usize::from(byte) % n
        }

        fn choose<T: Copy>(&mut self, items: &[T]) -> T {
            items[self.pick(items.len())]
        }
    }

    #[derive(Clone, Default)]
    struct Scope {
        vars: Vec<(String, Kind)>,
        funcs: Vec<(String, usize)>,
        next: usize,
    }

    struct Builder<'a> {
        tape: Tape<'a>,
        scope: Scope,
    }

    impl<'a> Builder<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self {
                tape: Tape { bytes, pos: 0 },
                scope: Scope::default(),
            }
        }

        fn fresh(&mut self, prefix: &str) -> String {
            self.scope.next += 1;
            format!("{prefix}{}", self.scope.next)
        }

        fn var(&mut self, kind: Kind) -> Option<String> {
            let names: Vec<String> = self
                .scope
                .vars
                .iter()
                .filter(|(_, var_kind)| *var_kind == kind)
                .map(|(name, _)| name.clone())
                .collect();
            if names.is_empty() {
                return None;
            }
            Some(names[self.tape.pick(names.len())].clone())
        }

        fn expr(&mut self, kind: Kind, depth: usize) -> Ex {
            match kind {
                Kind::Str => self.str_expr(depth),
                Kind::List => self.list_expr(depth),
                Kind::Dict => self.dict_expr(depth),
            }
        }

        fn lit(&mut self) -> Ex {
            Ex::Lit(self.tape.choose(&STRINGS))
        }

        fn str_expr(&mut self, depth: usize) -> Ex {
            let choice = if depth >= 3 { 0 } else { self.tape.pick(4) };
            match choice {
                1 => match self.var(Kind::Str) {
                    Some(name) => Ex::Var(name),
                    None => self.lit(),
                },
                2 => Ex::Add(
                    Box::new(self.str_expr(depth + 1)),
                    Box::new(self.str_expr(depth + 1)),
                ),
                _ => self.lit(),
            }
        }

        fn list_literal(&mut self, depth: usize) -> Ex {
            let len = self.tape.pick(4);
            Ex::List(None, (0..len).map(|_| self.str_expr(depth + 1)).collect())
        }

        fn list_expr(&mut self, depth: usize) -> Ex {
            let choice = if depth >= 3 { 0 } else { self.tape.pick(9) };
            let next = depth + 1;
            match choice {
                1 => match self.var(Kind::List) {
                    Some(name) => Ex::Var(name),
                    None => self.list_literal(depth),
                },
                2 => Ex::Add(
                    Box::new(self.list_expr(next)),
                    Box::new(self.list_expr(next)),
                ),
                3 => {
                    let spread = self.list_expr(next);
                    Ex::List(Some(Box::new(spread)), vec![self.str_expr(next)])
                },
                4 => Ex::Comp(Box::new(self.list_expr(next)), self.tape.choose(&STRINGS)),
                5 => Ex::Copy(Box::new(self.list_expr(next))),
                6 => match self.var(Kind::Dict) {
                    Some(name) => Ex::Sub(Box::new(Ex::Var(name)), self.tape.choose(&KEYS)),
                    None => self.list_literal(depth),
                },
                7 if !self.scope.funcs.is_empty() => {
                    let (name, arity) =
                        self.scope.funcs[self.tape.pick(self.scope.funcs.len())].clone();
                    Ex::Call(name, (0..arity).map(|_| self.list_expr(next)).collect())
                },
                _ => self.list_literal(depth),
            }
        }

        /// A dict variable is only spread, never aliased (see the module doc).
        fn dict_expr(&mut self, depth: usize) -> Ex {
            let choice = if depth >= 3 { 0 } else { self.tape.pick(3) };
            let base = match choice {
                1 => self.var(Kind::Dict).map(|name| Box::new(Ex::Var(name))),
                2 => return Ex::DictComp(Box::new(self.list_expr(depth + 1))),
                _ => None,
            };
            let len = self.tape.pick(4);
            let pairs = (0..len)
                .map(|_| (self.tape.choose(&KEYS), self.list_expr(depth + 1)))
                .collect();
            Ex::Dict(base, pairs)
        }

        fn assign_new(&mut self) -> St {
            let kind = self
                .tape
                .choose(&[Kind::Str, Kind::List, Kind::List, Kind::Dict]);
            let value = self.expr(kind, 0);
            let name = self.fresh("v");
            self.scope.vars.push((name.clone(), kind));
            St::Assign(name, value)
        }

        fn stmt(&mut self) -> St {
            match self.tape.pick(6) {
                1 => match self.var(Kind::List) {
                    Some(name) => {
                        let value = self.list_expr(1);
                        St::Assign(
                            name.clone(),
                            Ex::Add(Box::new(Ex::Var(name)), Box::new(value)),
                        )
                    },
                    None => self.assign_new(),
                },
                2 => match self.var(Kind::Str) {
                    Some(name) => St::AugAdd(name, self.str_expr(1)),
                    None => self.assign_new(),
                },
                3 => match self.var(Kind::Dict) {
                    Some(name) => St::SetItem(name, self.tape.choose(&KEYS), self.list_expr(1)),
                    None => self.assign_new(),
                },
                4 => match self.var(Kind::Dict) {
                    Some(name) => St::Del(name, self.tape.choose(&KEYS)),
                    None => self.assign_new(),
                },
                5 => self.def(),
                _ => self.assign_new(),
            }
        }

        fn def(&mut self) -> St {
            let name = self.fresh("f");
            let params: Vec<String> = (0..self.tape.pick(3))
                .map(|index| format!("{name}_p{index}"))
                .collect();
            let globals = self.scope.vars.clone();
            self.scope
                .vars
                .extend(params.iter().map(|param| (param.clone(), Kind::List)));
            let body = self.list_expr(1);
            self.scope.vars = globals;
            self.scope.funcs.push((name.clone(), params.len()));
            St::Def(name, params, body)
        }

        fn stmts(&mut self) -> Vec<St> {
            (0..self.tape.pick(8)).map(|_| self.stmt()).collect()
        }
    }

    fn py_insert(items: &mut Vec<(String, Py)>, key: &str, value: Py) {
        match items.iter_mut().find(|(existing, _)| existing == key) {
            Some((_, slot)) => *slot = value,
            None => items.push((key.to_owned(), value)),
        }
    }

    /// What `CPython` computes; `None` where it raises (`KeyError`).
    #[derive(Default)]
    struct Model {
        globals: HashMap<String, Py>,
        funcs: HashMap<String, (Vec<String>, Ex)>,
    }

    impl Model {
        fn run(stmts: &[St]) -> Option<Self> {
            let mut model = Self::default();
            for stmt in stmts {
                model.exec(stmt)?;
            }
            Some(model)
        }

        fn exec(&mut self, stmt: &St) -> Option<()> {
            match stmt {
                St::Assign(name, value) => {
                    let value = self.eval(value, &self.globals)?;
                    self.globals.insert(name.clone(), value);
                },
                St::AugAdd(name, value) => {
                    let sum = Ex::Add(Box::new(Ex::Var(name.clone())), Box::new(value.clone()));
                    let value = self.eval(&sum, &self.globals)?;
                    self.globals.insert(name.clone(), value);
                },
                St::SetItem(name, key, value) => {
                    let value = self.eval(value, &self.globals)?;
                    let Py::Dict(items) = self.globals.get_mut(name)? else {
                        return None;
                    };
                    py_insert(items, key, value);
                },
                St::Del(name, key) => {
                    let Py::Dict(items) = self.globals.get_mut(name)? else {
                        return None;
                    };
                    let index = items.iter().position(|(existing, _)| existing == key)?;
                    items.remove(index);
                },
                St::Def(name, params, body) => {
                    self.funcs
                        .insert(name.clone(), (params.clone(), body.clone()));
                },
            }
            Some(())
        }

        fn strings(&self, iter: &Ex, env: &HashMap<String, Py>) -> Option<Vec<String>> {
            let Py::List(items) = self.eval(iter, env)? else {
                return None;
            };
            items
                .into_iter()
                .map(|item| match item {
                    Py::Str(text) => Some(text),
                    _ => None,
                })
                .collect()
        }

        fn eval(&self, expr: &Ex, env: &HashMap<String, Py>) -> Option<Py> {
            Some(match expr {
                Ex::Lit(text) => Py::Str((*text).to_owned()),
                Ex::Var(name) => env.get(name)?.clone(),
                Ex::Add(left, right) => match (self.eval(left, env)?, self.eval(right, env)?) {
                    (Py::Str(left), Py::Str(right)) => Py::Str(left + &right),
                    (Py::List(mut left), Py::List(right)) => {
                        left.extend(right);
                        Py::List(left)
                    },
                    _ => return None,
                },
                Ex::List(spread, items) => {
                    let mut values = match spread {
                        Some(spread) => match self.eval(spread, env)? {
                            Py::List(values) => values,
                            _ => return None,
                        },
                        None => Vec::new(),
                    };
                    for item in items {
                        values.push(self.eval(item, env)?);
                    }
                    Py::List(values)
                },
                Ex::Comp(iter, suffix) => Py::List(
                    self.strings(iter, env)?
                        .into_iter()
                        .map(|text| Py::Str(text + suffix))
                        .collect(),
                ),
                Ex::Copy(inner) => self.eval(inner, env)?,
                Ex::Dict(base, pairs) => self.eval_dict(base.as_deref(), pairs, env)?,
                Ex::DictComp(iter) => {
                    let mut items = Vec::new();
                    for key in self.strings(iter, env)? {
                        let value = Py::List(vec![Py::Str(key.clone())]);
                        py_insert(&mut items, &key, value);
                    }
                    Py::Dict(items)
                },
                Ex::Sub(base, key) => match self.eval(base, env)? {
                    Py::Dict(items) => items.into_iter().find(|(existing, _)| existing == key)?.1,
                    _ => return None,
                },
                Ex::Call(name, args) => {
                    let (params, body) = self.funcs.get(name)?;
                    // A helper sees module globals and its own parameters.
                    let mut scope = self.globals.clone();
                    for (param, arg) in params.iter().zip(args) {
                        scope.insert(param.clone(), self.eval(arg, env)?);
                    }
                    self.eval(body, &scope)?
                },
            })
        }

        fn eval_dict(
            &self,
            base: Option<&Ex>,
            pairs: &[(&'static str, Ex)],
            env: &HashMap<String, Py>,
        ) -> Option<Py> {
            let mut items = match base {
                Some(base) => match self.eval(base, env)? {
                    Py::Dict(items) => items,
                    _ => return None,
                },
                None => Vec::new(),
            };
            for (key, value) in pairs {
                let value = self.eval(value, env)?;
                py_insert(&mut items, key, value);
            }
            Some(Py::Dict(items))
        }
    }

    fn render(expr: &Ex) -> String {
        let join = |items: &[Ex]| items.iter().map(render).collect::<Vec<_>>().join(", ");
        match expr {
            Ex::Lit(text) => format!("'{text}'"),
            Ex::Var(name) => name.clone(),
            Ex::Add(left, right) => format!("({} + {})", render(left), render(right)),
            Ex::List(None, items) => format!("[{}]", join(items)),
            Ex::List(Some(spread), items) => format!("[*{}, {}]", render(spread), join(items)),
            Ex::Comp(iter, suffix) => format!("[x + '{suffix}' for x in {}]", render(iter)),
            Ex::Copy(inner) => format!("list({})", render(inner)),
            Ex::Dict(base, pairs) => {
                let mut parts: Vec<String> = base
                    .iter()
                    .map(|base| format!("**{}", render(base)))
                    .collect();
                parts.extend(
                    pairs
                        .iter()
                        .map(|(key, value)| format!("'{key}': {}", render(value))),
                );
                format!("{{{}}}", parts.join(", "))
            },
            Ex::DictComp(iter) => format!("{{k: [k] for k in {}}}", render(iter)),
            Ex::Sub(base, key) => format!("{}['{key}']", render(base)),
            Ex::Call(name, args) => format!("{name}({})", join(args)),
        }
    }

    /// Layout-only variations that must not change the result.
    #[derive(Clone, Copy)]
    struct Style(u8);

    impl Style {
        const PLAIN: Self = Self(0);

        const fn has(self, bit: u8) -> bool {
            self.0 & bit != 0
        }

        fn rhs(self, expr: &Ex) -> String {
            let text = render(expr);
            match self.0 & 3 {
                1 => format!("(\n    {text}\n)"),
                2 => format!("\\\n    {text}"),
                _ => text,
            }
        }

        fn stmt(self, stmt: &St) -> String {
            match stmt {
                St::Assign(name, value) => format!("{name} = {}", self.rhs(value)),
                St::AugAdd(name, value) => format!("{name} += {}", self.rhs(value)),
                St::SetItem(name, key, value) => format!("{name}['{key}'] = {}", self.rhs(value)),
                St::Del(name, key) => format!("del {name}['{key}']"),
                St::Def(name, params, body) => {
                    let doc = if self.has(16) {
                        "    \"\"\"Helper.\"\"\"\n"
                    } else {
                        ""
                    };
                    format!(
                        "def {name}({}):\n{doc}    return {}",
                        params.join(", "),
                        self.rhs(body)
                    )
                },
            }
        }

        fn program(self, stmts: &[St], install: &Ex, extras: &Ex) -> String {
            let mut out = String::new();
            if self.has(8) {
                out.push_str("# generated\nfrom setuptools import setup\n\n");
            }
            for stmt in stmts {
                out.push_str(&self.stmt(stmt));
                if self.has(4) {
                    out.push_str("  # note");
                }
                out.push('\n');
                if self.has(8) {
                    out.push_str("\n# between\n");
                }
            }
            let _ = write!(
                out,
                "setup(\n    install_requires={},\n    extras_require={},\n)\n",
                render(install),
                render(extras)
            );
            if self.has(32) {
                out = out.replace('\n', "\r\n");
            }
            out
        }
    }

    fn evaluate(source: &str) -> Option<SetupCall> {
        let stmts = parse_module(source)?;
        evaluate_setup_call(Path::new("."), &stmts)
    }

    /// The evaluator's value when it is fully known.
    fn known(value: Option<&Value>) -> Option<Py> {
        match value? {
            Value::Str(text) => Some(Py::Str(text.clone())),
            Value::List(items) => items
                .iter()
                .map(|item| known(Some(item)))
                .collect::<Option<_>>()
                .map(Py::List),
            Value::Dict(items) => items
                .iter()
                .map(|(key, value)| Some((key.clone(), known(Some(value))?)))
                .collect::<Option<_>>()
                .map(Py::Dict),
            Value::RequirementsFile(_) | Value::Unknown => None,
        }
    }

    fn py_strings(value: &Py, out: &mut Vec<String>) {
        match value {
            Py::Str(text) => out.push(text.clone()),
            Py::List(items) => items.iter().for_each(|item| py_strings(item, out)),
            Py::Dict(_) => {},
        }
    }

    fn requirement_texts(value: Option<&Value>) -> Option<Vec<String>> {
        let items = dependency_items(value?);
        items.complete.then(|| {
            items
                .items
                .into_iter()
                .filter_map(|item| match item {
                    DependencyItem::Requirement(text) => Some(text),
                    DependencyItem::RequirementsFile(_) => None,
                })
                .collect()
        })
    }

    fn indented(stmt: &St) -> String {
        Style::PLAIN
            .stmt(stmt)
            .lines()
            .fold(String::new(), |mut out, line| {
                let _ = writeln!(out, "    {line}");
                out
            })
    }

    /// `(extra, requirement)` pairs.
    type ExtraPairs = Vec<(String, String)>;

    /// Install requirements and extra pairs `CPython` reports.
    fn model_result(stmts: &[St], install: &Ex, extras: &Ex) -> Option<(Vec<String>, ExtraPairs)> {
        let model = Model::run(stmts)?;
        let mut install_strings = Vec::new();
        py_strings(&model.eval(install, &model.globals)?, &mut install_strings);
        let Py::Dict(items) = model.eval(extras, &model.globals)? else {
            return None;
        };
        let mut pairs = Vec::new();
        for (key, value) in items {
            let mut strings = Vec::new();
            py_strings(&value, &mut strings);
            pairs.extend(strings.into_iter().map(|text| (key.clone(), text)));
        }
        Some((install_strings, pairs))
    }

    const MUTATIONS: [&str; 5] = [
        ".append('z')",
        ".extend(['z'])",
        ".insert(0, 'z')",
        ".clear()",
        ".remove('a')",
    ];

    /// Statements outside the modelled subset, applied to a variable of any kind.
    const DISRUPTIONS: [&str; 12] = [
        "for it in {v}:\n    {v} = {v} + [it]",
        "while COND:\n    {v}.append('x')",
        "{v} = {v} + {v}",
        "{v} = load({v})",
        "try:\n    {v} = {v} + ['t']\nexcept ImportError:\n    {v} = []",
        "{v}[{v}] = {v}",
        "del {v}",
        "{v}, {v} = {v}",
        "{v} = [{v} for {v} in {v}]",
        "{v} = {{**{v}, **{v}}}",
        "{v}['a'].append({v})",
        "if {v}:\n    del {v}['a']\nelif COND:\n    {v} += {v}\nelse:\n    {v} = {v}['a']['b']",
    ];

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn evaluation_matches_python_model(
            tape in prop::collection::vec(any::<u8>(), 0..160),
            style in any::<u8>(),
        ) {
            let mut builder = Builder::new(&tape);
            let stmts = builder.stmts();
            let install = builder.list_expr(0);
            let extras = builder.dict_expr(0);
            let Some(model) = Model::run(&stmts) else {
                return Ok(());
            };
            let (Some(want_install), Some(want_extras)) = (
                model.eval(&install, &model.globals),
                model.eval(&extras, &model.globals),
            ) else {
                return Ok(());
            };
            let source = Style(style).program(&stmts, &install, &extras);
            let call = evaluate(&source).expect("generated setup.py has a setup() call");
            prop_assert_eq!(known(call.keyword("install_requires")), Some(want_install), "{}", source);
            prop_assert_eq!(known(call.keyword("extras_require")), Some(want_extras), "{}", source);
        }

        #[test]
        fn if_else_keeps_what_every_branch_declares(
            tape in prop::collection::vec(any::<u8>(), 0..160),
            has_else in any::<bool>(),
        ) {
            let mut builder = Builder::new(&tape);
            let prefix = builder.stmts();
            let saved = builder.scope.clone();
            let first = builder.stmt();
            let next = builder.scope.next;
            builder.scope = Scope { next, ..saved.clone() };
            let second = builder.stmt();
            let install = saved
                .vars
                .iter()
                .filter(|(_, kind)| *kind == Kind::List)
                .fold(Ex::List(None, Vec::new()), |sum, (name, _)| {
                    Ex::Add(Box::new(sum), Box::new(Ex::Var(name.clone())))
                });
            let extras = saved
                .vars
                .iter()
                .find(|(_, kind)| *kind == Kind::Dict)
                .map_or(Ex::Dict(None, Vec::new()), |(name, _)| Ex::Var(name.clone()));

            let mut runs = vec![[prefix.clone(), vec![first.clone()]].concat()];
            let mut source = prefix.iter().map(|stmt| Style::PLAIN.stmt(stmt) + "\n").collect::<String>();
            source.push_str("if COND:\n");
            source.push_str(&indented(&first));
            if has_else {
                runs.push([prefix, vec![second.clone()]].concat());
                source.push_str("else:\n");
                source.push_str(&indented(&second));
            } else {
                runs.push(prefix);
            }
            let _ = writeln!(source, "setup(install_requires={}, extras_require={})", render(&install), render(&extras));
            let Some(expected) = runs
                .iter()
                .map(|stmts| model_result(stmts, &install, &extras))
                .collect::<Option<Vec<_>>>()
            else {
                return Ok(());
            };

            let call = evaluate(&source).expect("generated setup.py has a setup() call");
            let got_install = requirement_texts(call.keyword("install_requires"));
            prop_assert!(got_install.is_some(), "install_requires partial:\n{}", source);
            let got_install = got_install.unwrap_or_default();
            let Some(Value::Dict(got_extras)) = call.keyword("extras_require") else {
                prop_assert!(false, "extras_require unreadable:\n{}", source);
                return Ok(());
            };
            for (install, pairs) in &expected {
                for text in install {
                    prop_assert!(got_install.contains(text), "{} missing:\n{}", text, source);
                }
                for (extra, text) in pairs {
                    let found = got_extras
                        .iter()
                        .filter(|(key, _)| key == extra)
                        .filter_map(|(_, value)| requirement_texts(Some(value)))
                        .any(|texts| texts.contains(text));
                    prop_assert!(found, "{}[{}] missing:\n{}", extra, text, source);
                }
            }
        }

        #[test]
        fn unfollowed_mutation_marks_the_value_partial(
            tape in prop::collection::vec(any::<u8>(), 0..120),
            mutation in prop::sample::select(MUTATIONS.as_slice()),
            through_dict in any::<bool>(),
        ) {
            let mut builder = Builder::new(&tape);
            let stmts = builder.stmts();
            // A program CPython rejects (`KeyError`) says nothing about taint,
            // and a missing key would fall to the requirement-table lookup.
            let Some(model) = Model::run(&stmts) else {
                return Ok(());
            };
            let target = match (through_dict, builder.var(Kind::Dict), builder.var(Kind::List)) {
                (true, Some(dict), _) => {
                    let key = builder.tape.choose(&KEYS);
                    let Some(Py::Dict(items)) = model.globals.get(&dict) else {
                        return Ok(());
                    };
                    if !items.iter().any(|(existing, value)| existing == key && matches!(value, Py::List(_))) {
                        return Ok(());
                    }
                    format!("{dict}['{key}']")
                },
                (_, _, Some(list)) => list,
                _ => return Ok(()),
            };
            let mut source = Style::PLAIN.program(&stmts, &Ex::List(None, Vec::new()), &Ex::Dict(None, Vec::new()));
            source.truncate(source.rfind("setup(").unwrap_or(source.len()));
            let _ = write!(source, "{target}{mutation}\nsetup(install_requires={target})\n");
            let call = evaluate(&source).expect("generated setup.py has a setup() call");
            let value = call.keyword("install_requires").expect("install_requires");
            prop_assert!(!dependency_items(value).complete, "complete after mutation:\n{}", source);
        }

        #[test]
        fn unmodelled_statements_never_panic_and_are_deterministic(
            tape in prop::collection::vec(any::<u8>(), 0..160),
            disruptions in prop::collection::vec((prop::sample::select(DISRUPTIONS.as_slice()), any::<prop::sample::Index>()), 1..4),
        ) {
            let mut builder = Builder::new(&tape);
            let stmts = builder.stmts();
            let names: Vec<String> = builder.scope.vars.iter().map(|(name, _)| name.clone()).collect();
            let mut source = Style::PLAIN.program(&stmts, &Ex::List(None, Vec::new()), &Ex::Dict(None, Vec::new()));
            source.truncate(source.rfind("setup(").unwrap_or(source.len()));
            let mut last = String::from("v0");
            for (template, index) in &disruptions {
                last = if names.is_empty() { last } else { names[index.index(names.len())].clone() };
                source.push_str(&template.replace("{v}", &last));
                source.push('\n');
            }
            let _ = writeln!(source, "setup(install_requires={last}, extras_require={last})");
            let first = evaluate(&source).expect("generated setup.py has a setup() call");
            let second = evaluate(&source).expect("generated setup.py has a setup() call");
            prop_assert_eq!(&first.keywords, &second.keywords);
            for (_, value) in &first.keywords {
                let _ = dependency_items(value);
            }
        }
    }

    #[test]
    fn del_name_forgets_the_value() {
        let call = evaluate("reqs = ['a']\ndel reqs\nsetup(install_requires=reqs)\n")
            .expect("setup() call");
        assert_eq!(known(call.keyword("install_requires")), None);
    }

    #[test]
    fn del_tuple_and_list_remove_every_key() {
        let call = evaluate(
            "extras = {'a': ['x'], 'b': ['y'], 'c': ['z']}\n\
             del (extras['a'], [extras['b']])\n\
             setup(extras_require=extras)\n",
        )
        .expect("setup() call");
        assert_eq!(
            known(call.keyword("extras_require")),
            Some(Py::Dict(vec![(
                "c".to_owned(),
                Py::List(vec![Py::Str("z".to_owned())])
            )]))
        );
    }

    #[test]
    fn del_in_loop_body_marks_the_value_partial() {
        let call = evaluate(
            "extras = {'a': ['x']}\nfor _ in COND:\n    del extras['a']\n\
             setup(extras_require=extras)\n",
        )
        .expect("setup() call");
        assert_eq!(known(call.keyword("extras_require")), None);
    }
}
