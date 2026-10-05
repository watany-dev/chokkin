//! Non-executing evaluation of the `setup.py` subset that builds dependency lists.
//!
//! Only literals, names, `+`, string-key subscripts, single-`return` helper
//! functions, and filter-free comprehensions are evaluated. Everything else is
//! [`Value::Unknown`], which callers treat as "could not be read" (#491).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ruff_python_ast::{Comprehension, Expr, ExprCall, Operator, Stmt, StmtFunctionDef, Suite};

use crate::path_util::rel_to_root;

use super::pep508_util::normalize_distribution_name;
use super::util::path_is_within_root;

const MAX_CALL_DEPTH: usize = 16;
const MAX_COMPREHENSION_ITEMS: usize = 10_000;

/// Statically evaluated Python value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Value {
    Str(String),
    List(Vec<Self>),
    Dict(Vec<(String, Self)>),
    /// A requirements file that a helper function reads at install time.
    RequirementsFile(PathBuf),
    Unknown,
}

/// One flattened entry of a dependency list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DependencyItem {
    Requirement(String),
    RequirementsFile(PathBuf),
}

/// Flattened dependency list; `complete` is `false` when part of it was unreadable.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct DependencyItems {
    pub items: Vec<DependencyItem>,
    pub complete: bool,
}

/// Evaluated keyword arguments of the `setup(...)` call.
#[derive(Debug, Default)]
pub(super) struct SetupCall {
    pub keywords: Vec<(String, Value)>,
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
        probed_missing: Vec::new(),
    };
    let keywords = evaluator.exec_block(stmts)?;
    Some(SetupCall {
        keywords,
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
    probed_missing: Vec<String>,
}

impl<'a> Evaluator<'a> {
    /// Run statements in order; every `if`/`try`/`with` branch is taken.
    fn exec_block(&mut self, stmts: &'a [Stmt]) -> Option<Vec<(String, Value)>> {
        for stmt in stmts {
            match stmt {
                Stmt::Expr(expr) => {
                    if let Expr::Call(call) = &*expr.value
                        && is_setup(&call.func)
                    {
                        return Some(self.setup_keywords(call));
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
                        add(self.eval(&assign.target), self.eval(&assign.value))
                    } else {
                        Value::Unknown
                    };
                    self.assign(&assign.target, value);
                },
                Stmt::FunctionDef(def) => {
                    self.functions.insert(def.name.id.to_string(), def);
                },
                Stmt::If(if_stmt) => {
                    if let Some(found) = self.exec_block(&if_stmt.body) {
                        return Some(found);
                    }
                    for clause in &if_stmt.elif_else_clauses {
                        if let Some(found) = self.exec_block(&clause.body) {
                            return Some(found);
                        }
                    }
                },
                Stmt::Try(try_stmt) => {
                    let handlers = try_stmt.handlers.iter().map(|handler| {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                        &handler.body
                    });
                    let blocks = std::iter::once(&try_stmt.body)
                        .chain(handlers)
                        .chain([&try_stmt.orelse, &try_stmt.finalbody]);
                    for block in blocks {
                        if let Some(found) = self.exec_block(block) {
                            return Some(found);
                        }
                    }
                },
                Stmt::With(with) => {
                    if let Some(found) = self.exec_block(&with.body) {
                        return Some(found);
                    }
                },
                _ => {},
            }
        }
        None
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
                    match items.iter_mut().find(|(existing, _)| *existing == key) {
                        Some((_, slot)) => *slot = value,
                        None => items.push((key, value)),
                    }
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
                        Some(Value::Str(key)) => items.push((key, self.eval(&item.value))),
                        None => {
                            if let Value::Dict(spread) = self.eval(&item.value) {
                                items.extend(spread);
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
                    Value::Unknown => self.requirement_table_lookup(&key),
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
                    items.push((key, value));
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
                    if next.len() >= MAX_COMPREHENSION_ITEMS {
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
            Some(def) => {
                let kwargs: Vec<(String, Value)> = call
                    .arguments
                    .keywords
                    .iter()
                    .filter_map(|keyword| {
                        let name = keyword.arg.as_ref()?.to_string();
                        Some((name, self.eval(&keyword.value)))
                    })
                    .collect();
                self.call_function(def, args.clone(), &kwargs)
            },
            None => Value::Unknown,
        };
        if value == Value::Unknown {
            self.requirements_file(&args)
        } else {
            value
        }
    }

    /// Inline a helper whose body is an optional docstring plus one `return`.
    fn call_function(
        &mut self,
        def: &'a StmtFunctionDef,
        args: Vec<Value>,
        kwargs: &[(String, Value)],
    ) -> Value {
        if self.depth >= MAX_CALL_DEPTH {
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
        let mut locals: Vec<(String, Value)> = Vec::new();
        let mut positional = args.into_iter();
        for param in parameters.posonlyargs.iter().chain(&parameters.args) {
            let name = param.parameter.name.id.to_string();
            let value = positional
                .next()
                .or_else(|| {
                    kwargs
                        .iter()
                        .find(|(keyword, _)| *keyword == name)
                        .map(|(_, value)| value.clone())
                })
                .or_else(|| param.default.as_deref().map(|default| self.eval(default)))
                .unwrap_or(Value::Unknown);
            locals.push((name, value));
        }
        if let Some(vararg) = &parameters.vararg {
            locals.push((
                vararg.name.id.to_string(),
                Value::List(positional.collect()),
            ));
        }
        for param in &parameters.kwonlyargs {
            let name = param.parameter.name.id.to_string();
            let value = kwargs
                .iter()
                .find(|(keyword, _)| *keyword == name)
                .map(|(_, value)| value.clone())
                .or_else(|| param.default.as_deref().map(|default| self.eval(default)))
                .unwrap_or(Value::Unknown);
            locals.push((name, value));
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
        if !is_requirements || relative.is_absolute() {
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

pub(super) fn is_setup(func: &Expr) -> bool {
    match func {
        Expr::Name(name) => name.id.as_str() == "setup",
        Expr::Attribute(attribute) => attribute.attr.as_str() == "setup",
        _ => false,
    }
}
