//! Non-executing evaluation of the `setup.py` subset that builds dependency lists.
//!
//! Only literals, names, `+`, string-key subscripts, single-`return` helper
//! functions, and filter-free comprehensions are evaluated. Everything else is
//! [`Value::Unknown`], which callers treat as "could not be read" (#491),
//! except the known project idioms in [`idioms`], tried just before that.

mod idioms;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use ruff_python_ast::{
    Comprehension, Expr, ExprCall, ExprSubscript, Operator, Stmt, StmtFunctionDef, Suite,
};

use crate::path_util::rel_to_root;

use super::setuptools_reqs::setuptools_requirement_items;
use super::util::path_is_within_root;

const MAX_CALL_DEPTH: usize = 16;
const MAX_COMPREHENSION_ITEMS: usize = 10_000;
/// Largest value kept, in nodes; `x = x + x` doubles without it.
const MAX_VALUE_NODES: usize = 100_000;
/// A string costs one node more per this many bytes, so long strings count
/// against the caps: `s = s + s` would otherwise double in one node until
/// the allocation fails.
const STR_BYTES_PER_NODE: usize = 32;
/// Total work allowed per file, so fan-out helpers and nested
/// comprehensions end as `Unknown` instead of running away.
const MAX_STEPS: usize = 5_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Value {
    Str(String),
    List(Vec<Self>, Identity),
    Dict {
        items: Vec<(String, Self)>,
        /// Entries we could not key or follow may also be there (#869).
        partial: bool,
        id: Identity,
    },
    /// A requirements file that a helper function reads at install time.
    RequirementsFile(PathBuf),
    Unknown,
}

/// The Python object a list or dict value copies, so that a change made
/// through one name reaches every alias (#565). Tuples have none: they
/// cannot change in place. `==` ignores it and compares contents.
#[derive(Debug, Clone, Copy)]
pub(super) struct Identity(pub(super) Option<usize>);

impl PartialEq for Identity {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Identity {}

impl Value {
    fn nodes(&self) -> usize {
        match self {
            Self::List(items, _) => 1 + items.iter().map(Self::nodes).sum::<usize>(),
            Self::Dict { items, .. } => {
                1 + items
                    .iter()
                    .map(|(key, value)| str_nodes(key.len()) - 1 + value.nodes())
                    .sum::<usize>()
            },
            Self::Str(text) => str_nodes(text.len()),
            Self::RequirementsFile(_) | Self::Unknown => 1,
        }
    }

    /// Mark a value that code we cannot follow may have changed. A
    /// container keeps what it read, as a dict literal keeps its string
    /// keys: code we cannot follow may add to it, and dropping what we read
    /// would undeclare it (#869).
    fn taint(&mut self) {
        match self {
            Self::List(items, _) => items.push(Self::Unknown),
            Self::Dict { partial, .. } => *partial = true,
            _ => *self = Self::Unknown,
        }
    }

    const fn object(&self) -> Option<usize> {
        match self {
            Self::List(_, id) | Self::Dict { id, .. } => id.0,
            _ => None,
        }
    }

    fn count_object(&self, object: usize) -> usize {
        if self.object() == Some(object) {
            return 1;
        }
        match self {
            Self::List(items, _) => items.iter().map(|item| item.count_object(object)).sum(),
            Self::Dict { items, .. } => items
                .iter()
                .map(|(_, value)| value.count_object(object))
                .sum(),
            _ => 0,
        }
    }

    fn replace_object(&mut self, object: usize, with: &Self) {
        if self.object() == Some(object) {
            *self = with.clone();
        } else {
            self.replace_within(object, with);
        }
    }

    fn replace_within(&mut self, object: usize, with: &Self) {
        match self {
            Self::List(items, _) => {
                for item in items {
                    item.replace_object(object, with);
                }
            },
            Self::Dict { items, .. } => {
                for (_, value) in items {
                    value.replace_object(object, with);
                }
            },
            _ => {},
        }
    }
}

/// The element an integer literal index (`0`, `-1`) names in `items`, as
/// `CPython` counts it. `None` for any other index, one out of range (`CPython`
/// raises), or one counted over an element that may stand for any number of
/// them (an unknown spread `*more`, a requirements file), which may shift it.
fn list_position(items: &[Value], index: &Expr) -> Option<usize> {
    let (negative, operand) = match index {
        Expr::UnaryOp(ruff_python_ast::ExprUnaryOp {
            op: ruff_python_ast::UnaryOp::USub,
            operand,
            ..
        }) => (true, &**operand),
        _ => (false, index),
    };
    let Expr::NumberLiteral(ruff_python_ast::ExprNumberLiteral {
        value: ruff_python_ast::Number::Int(int),
        ..
    }) = operand
    else {
        return None;
    };
    let offset = int.as_usize()?;
    let (position, counted) = if negative {
        let position = items.len().checked_sub(offset).filter(|_| offset > 0)?;
        (position, &items[position..])
    } else {
        (offset, items.get(..=offset)?)
    };
    counted
        .iter()
        .all(|item| matches!(item, Value::Str(_) | Value::List(..) | Value::Dict { .. }))
        .then_some(position)
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
    /// Every requirement string in the lists `deps["name"]` looked up
    /// (transformers `_deps`), whether `setup()` uses them or not.
    pub table_requirements: BTreeSet<String>,
}

impl SetupCall {
    pub(super) fn keyword(&self, name: &str) -> Option<&Value> {
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
        objects: 0,
        probed_missing: Vec::new(),
        table_requirements: BTreeSet::new(),
        ordered: HashSet::new(),
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
        table_requirements: evaluator.table_requirements,
    })
}

/// Flatten a dependency-list value into requirement strings and file references.
///
/// setuptools reads a string `install_requires` and the string items of a
/// list with `parse_strings` (#965): the lines of every string are read
/// together, so a comment is cut and a `\` continuation joins across list
/// items too (`["requests \\", ">=2"]` is one requirement). A file
/// reference or an unreadable item stands for lines chokkin has not read, so
/// it ends the run.
pub(super) fn dependency_items(value: &Value) -> DependencyItems {
    let mut result = DependencyItems {
        items: Vec::new(),
        complete: true,
    };
    let mut run = Vec::new();
    collect(value, &mut run, &mut result);
    push_parsed_strings(&mut run, &mut result.items);
    result
}

fn collect<'v>(value: &'v Value, run: &mut Vec<&'v str>, result: &mut DependencyItems) {
    match value {
        Value::Str(text) => run.push(text),
        Value::List(items, _) => {
            for item in items {
                collect(item, run, result);
            }
        },
        Value::RequirementsFile(path) => {
            push_parsed_strings(run, &mut result.items);
            result
                .items
                .push(DependencyItem::RequirementsFile(path.clone()));
        },
        Value::Dict { .. } | Value::Unknown => {
            push_parsed_strings(run, &mut result.items);
            result.complete = false;
        },
    }
}

fn push_parsed_strings(run: &mut Vec<&str>, items: &mut Vec<DependencyItem>) {
    items.extend(
        setuptools_requirement_items(run.iter().flat_map(|text| text.lines()))
            .into_iter()
            .map(|item| DependencyItem::Requirement(item.text)),
    );
    run.clear();
}

struct Evaluator<'a> {
    root: &'a Path,
    env: HashMap<String, Value>,
    functions: HashMap<String, &'a StmtFunctionDef>,
    depth: usize,
    steps: usize,
    objects: usize,
    probed_missing: Vec<String>,
    /// Items of the lists [`idioms::requirement_table_lookup`] found an entry
    /// in, read at lookup time: a later rebinding does not change what was
    /// looked up.
    table_requirements: BTreeSet<String>,
    /// The list objects whose element order we know, so that an index
    /// names one element (#868): built by a list literal and since changed
    /// only in ways we follow exactly. A copy (`list(x)`, `x + y`, a
    /// comprehension) gets a new object outside it.
    ordered: HashSet<usize>,
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
                Stmt::AugAssign(assign) => self.aug_assign(assign),
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
                        self.taint_target(&for_stmt.target, false);
                    }
                    self.taint_block(body);
                    self.taint_block(orelse);
                },
                _ => {},
            }
        }
        None
    }

    fn aug_assign(&mut self, assign: &ruff_python_ast::StmtAugAssign) {
        let target = self.eval(&assign.target);
        let object = target.object();
        let value = if assign.op == Operator::Add && !matches!(target, Value::Dict { .. }) {
            let added = self.eval(&assign.value);
            if !added
                .object()
                .is_some_and(|added| self.ordered.contains(&added))
                && let Some(object) = object
            {
                self.ordered.remove(&object);
            }
            let sum = add(target, added, Identity(object));
            self.checked(sum)
        } else {
            Value::Unknown
        };
        match object {
            // `list += ...` extends the object every alias names.
            Some(object) => self.propagate(object, value),
            None => self.assign(&assign.target, value),
        }
    }

    /// Run each branch from the current state and keep the union of what
    /// they assign; `fallthrough` adds the state where no branch ran.
    fn exec_branches(
        &mut self,
        branches: &[&'a [Stmt]],
        fallthrough: bool,
    ) -> Option<&'a ExprCall> {
        // Every branch copies the environment. Once the budget is spent no
        // stored value is read again, so nested branches drop them instead.
        let copies = branches.len() + usize::from(fallthrough) + 1;
        if !self.spend(copies.saturating_mul(self.env_nodes())) {
            self.env.clear();
        }
        let saved = self.env.clone();
        let mut merged: Option<HashMap<String, Value>> = fallthrough.then(|| saved.clone());
        for branch in branches {
            self.env.clone_from(&saved);
            if let Some(found) = self.exec_block(branch) {
                return Some(found);
            }
            let env = std::mem::take(&mut self.env);
            merged = Some(match merged {
                Some(merged) => self.merge_env(merged, env),
                None => env,
            });
        }
        self.env = merged.unwrap_or(saved);
        None
    }

    /// State after one of two branches ran: lists and dicts keep every entry
    /// either branch gave, so an optional dependency still counts as declared.
    fn merge_env(
        &mut self,
        mut left: HashMap<String, Value>,
        right: HashMap<String, Value>,
    ) -> HashMap<String, Value> {
        let mut joined = HashMap::new();
        for (name, value) in right {
            let merged = match left.remove(&name) {
                Some(existing) => self.merge(existing, value, &mut joined),
                None => value,
            };
            left.insert(name, merged);
        }
        left
    }

    /// `joined` maps each pair of objects already merged to the object
    /// standing for both, so names aliased in both branches stay aliased.
    fn merge(
        &mut self,
        left: Value,
        right: Value,
        joined: &mut HashMap<(Option<usize>, Option<usize>), Identity>,
    ) -> Value {
        // A name bound to a different object in each branch gets a new one:
        // a later in-place change through either object's aliases would
        // otherwise overwrite the entries the other branch gave.
        let pair = (left.object(), right.object());
        let identity =
            (pair.0 != pair.1).then(|| *joined.entry(pair).or_insert_with(|| self.fresh()));
        match (left, right) {
            (left, right) if left == right && identity.is_none() => left,
            (Value::List(mut left, kept), Value::List(right, _)) => {
                // Either branch's order may hold, so neither does; a new
                // object is outside `ordered` already.
                if identity.is_none()
                    && let Some(object) = kept.0
                {
                    self.ordered.remove(&object);
                }
                for item in right {
                    if !left.contains(&item) {
                        left.push(item);
                    }
                }
                Value::List(left, identity.unwrap_or(kept))
            },
            (
                Value::Dict {
                    items: mut left,
                    partial: left_partial,
                    id: kept,
                },
                Value::Dict {
                    items: right,
                    partial: right_partial,
                    ..
                },
            ) => {
                for (key, value) in right {
                    match left.iter().position(|(existing, _)| *existing == key) {
                        Some(index) => {
                            let (_, existing) = left.remove(index);
                            let merged = self.merge(existing, value, joined);
                            left.insert(index, (key, merged));
                        },
                        None => left.push((key, value)),
                    }
                }
                Value::Dict {
                    items: left,
                    partial: left_partial || right_partial,
                    id: identity.unwrap_or(kept),
                }
            },
            _ => Value::Unknown,
        }
    }

    /// `reqs.append(...)` and friends change a value we only track by assignment.
    fn taint_mutation(&mut self, call: &ExprCall) {
        if let Expr::Attribute(attribute) = &*call.func {
            self.taint_place(&attribute.value);
        }
    }

    /// Apply `change` to the object `place` names, seen through every alias;
    /// `false` when `place` names no stored value.
    fn mutate(&mut self, place: &Expr, change: impl FnOnce(&mut Value)) -> bool {
        if self.exhausted() {
            return true;
        }
        let Some(slot) = self.place_mut(place) else {
            return false;
        };
        let object = slot.object();
        change(slot);
        if let Some(object) = object {
            let value = slot.clone();
            self.propagate(object, value);
        }
        true
    }

    /// Give every copy of `object` its state `value` after an in-place change.
    fn propagate(&mut self, object: usize, mut value: Value) {
        // Once the budget is spent every later `eval`, `setup()`'s keywords
        // included, is `Unknown`, so stale copies can no longer be read.
        if self.exhausted() {
            return;
        }
        // An object holding itself (`reqs += [reqs]`) has no finite value.
        value.replace_within(object, &Value::Unknown);
        let copies: usize = self
            .env
            .values()
            .map(|stored| stored.count_object(object))
            .sum();
        if !self.spend(self.env_nodes()) {
            return;
        }
        let added = copies.saturating_mul(value.nodes());
        if added > MAX_VALUE_NODES || !self.spend(added) {
            value = Value::Unknown;
        }
        for stored in self.env.values_mut() {
            stored.replace_object(object, &value);
        }
    }

    fn delete(&mut self, targets: &[Expr]) {
        for target in targets {
            match target {
                Expr::Name(name) => {
                    self.env.remove(name.id.as_str());
                },
                Expr::Subscript(subscript) => {
                    let key = self.eval(&subscript.slice);
                    let position = self.list_index(&subscript.value, &subscript.slice);
                    let found = self.mutate(&subscript.value, |base| match (base, &key) {
                        (Value::Dict { items, .. }, Value::Str(key)) => {
                            items.retain(|(existing, _)| existing != key);
                        },
                        (Value::List(items, _), _) => match position {
                            Some(position) => {
                                items.remove(position);
                            },
                            None => items.push(Value::Unknown),
                        },
                        (base, _) => base.taint(),
                    });
                    if !found {
                        self.taint_place(&subscript.value);
                    }
                },
                Expr::Tuple(ruff_python_ast::ExprTuple { elts, .. })
                | Expr::List(ruff_python_ast::ExprList { elts, .. }) => self.delete(elts),
                _ => {},
            }
        }
    }

    /// The element `place[index]` names in a list of known order, as
    /// [`list_position`] reads it. `None` otherwise, and the list's order
    /// is then lost: the caller adds an element we cannot place.
    fn list_index(&mut self, place: &Expr, index: &Expr) -> Option<usize> {
        let Some(Value::List(items, Identity(Some(object)))) = self.place_mut(place) else {
            return None;
        };
        let object = *object;
        let position = list_position(items, index).filter(|_| self.ordered.contains(&object));
        if position.is_none() {
            self.ordered.remove(&object);
        }
        position
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
                    Value::Dict { items, .. } => items
                        .iter_mut()
                        .find(|(existing, _)| *existing == key)
                        .map(|(_, value)| value),
                    _ => None,
                }
            },
            _ => None,
        }
    }

    /// A change we cannot follow to the object `place` names, or failing
    /// that to the dict holding it.
    fn taint_place(&mut self, place: &Expr) {
        if let Some(object) = self.place_mut(place).and_then(|value| value.object()) {
            self.ordered.remove(&object);
        }
        if !self.mutate(place, Value::taint)
            && let Expr::Subscript(subscript) = place
        {
            self.taint_place(&subscript.value);
        }
    }

    /// A loop may rebind `target` or, `in_place`, change what it names.
    fn taint_target(&mut self, target: &Expr, in_place: bool) {
        match target {
            Expr::Name(_) if in_place => self.taint_place(target),
            // The aliases keep the old object whenever the loop body runs.
            // Unless it never runs: then a later in-place change through
            // this name reaching them is missed.
            Expr::Name(name) => {
                let identity = self.fresh();
                if let Some(value) = self.env.get_mut(name.id.as_str()) {
                    value.taint();
                    if let Value::List(_, kept) | Value::Dict { id: kept, .. } = value {
                        *kept = identity;
                    }
                }
            },
            Expr::Subscript(subscript) => self.taint_place(&subscript.value),
            Expr::Tuple(ruff_python_ast::ExprTuple { elts, .. })
            | Expr::List(ruff_python_ast::ExprList { elts, .. }) => {
                for elt in elts {
                    self.taint_target(elt, in_place);
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
                        self.taint_target(target, false);
                    }
                },
                Stmt::AnnAssign(ruff_python_ast::StmtAnnAssign { target, .. }) => {
                    self.taint_target(target, false);
                },
                Stmt::AugAssign(ruff_python_ast::StmtAugAssign { target, .. }) => {
                    self.taint_target(target, true);
                },
                Stmt::Delete(delete) => {
                    for target in &delete.targets {
                        self.taint_target(target, false);
                    }
                },
                Stmt::For(for_stmt) => {
                    self.taint_target(&for_stmt.target, false);
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

    const fn exhausted(&self) -> bool {
        self.steps > MAX_STEPS
    }

    fn fresh(&mut self) -> Identity {
        self.objects += 1;
        Identity(Some(self.objects))
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
                    "name"
                        | "version"
                        | "python_requires"
                        | "install_requires"
                        | "extras_require"
                        | "entry_points"
                        | "packages"
                        | "py_modules"
                        | "package_dir"
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
            // A key that is not a string leaves the dict partial as in a dict
            // literal: an added entry cannot undeclare the known ones (#827,
            // #869). A list's element is replaced only where `list_index` reads
            // the index (#868). Any other base leaves it partial.
            Expr::Subscript(subscript) => {
                let key = self.eval(&subscript.slice);
                let position = self.list_index(&subscript.value, &subscript.slice);
                let found = self.mutate(&subscript.value, |base| match (base, key) {
                    (Value::Dict { items, .. }, Value::Str(key)) => insert_entry(items, key, value),
                    (Value::List(items, _), _) => {
                        if let Some(position) = position {
                            items[position] = value;
                        } else {
                            // Whichever element it replaced, the value is in
                            // the list. A slice spreads it (`'ab'` as `a`,
                            // `b`), which we do not model. `Unknown` first so
                            // the value does not join a `\` continuation.
                            items.push(Value::Unknown);
                            if !matches!(&*subscript.slice, Expr::Slice(_)) {
                                items.push(value);
                            }
                        }
                    },
                    (base, _) => base.taint(),
                });
                if !found {
                    self.taint_place(&subscript.value);
                }
            },
            Expr::Tuple(ruff_python_ast::ExprTuple { elts, .. })
            | Expr::List(ruff_python_ast::ExprList { elts, .. }) => {
                let values = match value {
                    Value::List(values, _) if values.len() == elts.len() => values,
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
            | Expr::Set(ruff_python_ast::ExprSet { elts, .. }) => {
                let items = self.eval_elements(elts);
                let identity = self.fresh();
                if let (Expr::List(_), Identity(Some(object))) = (expr, identity) {
                    self.ordered.insert(object);
                }
                Value::List(items, identity)
            },
            Expr::Tuple(tuple) => Value::List(self.eval_elements(&tuple.elts), Identity(None)),
            Expr::Dict(dict) => self.eval_dict(dict),
            Expr::Name(name) => self
                .env
                .get(name.id.as_str())
                .cloned()
                .unwrap_or(Value::Unknown),
            Expr::BinOp(binop) if binop.op == Operator::Add => {
                let (left, right) = (self.eval(&binop.left), self.eval(&binop.right));
                let tuples = [&left, &right]
                    .iter()
                    .all(|value| matches!(value, Value::List(_, Identity(None))));
                let identity = if tuples { Identity(None) } else { self.fresh() };
                add(left, right, identity)
            },
            Expr::Subscript(subscript) => self.eval_subscript(subscript),
            Expr::Call(call) => self.eval_call(call),
            Expr::ListComp(ruff_python_ast::ExprListComp {
                elt, generators, ..
            })
            | Expr::SetComp(ruff_python_ast::ExprSetComp {
                elt, generators, ..
            })
            | Expr::Generator(ruff_python_ast::ExprGenerator {
                elt, generators, ..
            }) => self.eval_list_comprehension(elt, generators),
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
                let mut partial = false;
                for (key, value) in pairs {
                    match key {
                        Value::Str(key) => insert_entry(&mut items, key, value),
                        _ => partial = true,
                    }
                }
                Value::Dict {
                    items,
                    partial,
                    id: self.fresh(),
                }
            },
            _ => Value::Unknown,
        }
    }

    /// An entry we cannot key, or a `**` spread we cannot read, leaves the
    /// dict partial and keeps the rest (#869).
    fn eval_dict(&mut self, dict: &ruff_python_ast::ExprDict) -> Value {
        let mut items: Vec<(String, Value)> = Vec::new();
        let mut partial = false;
        for item in &dict.items {
            match item.key.as_ref().map(|key| self.eval(key)) {
                Some(Value::Str(key)) => {
                    let value = self.eval(&item.value);
                    insert_entry(&mut items, key, value);
                },
                None => match self.eval(&item.value) {
                    Value::Dict {
                        items: spread,
                        partial: spread_partial,
                        ..
                    } => {
                        partial |= spread_partial;
                        for (key, value) in spread {
                            insert_entry(&mut items, key, value);
                        }
                    },
                    _ => partial = true,
                },
                Some(_) => partial = true,
            }
        }
        Value::Dict {
            items,
            partial,
            id: self.fresh(),
        }
    }

    fn eval_subscript(&mut self, subscript: &ExprSubscript) -> Value {
        let base = self.eval(&subscript.value);
        let Value::Str(key) = self.eval(&subscript.slice) else {
            return Value::Unknown;
        };
        let base = match base {
            Value::Dict { items, partial, .. } => {
                match items.into_iter().find(|(existing, _)| *existing == key) {
                    Some((_, value)) => return value,
                    None if partial => Value::Unknown,
                    None => return Value::Unknown,
                }
            },
            other => other,
        };
        match base {
            // Only a table this file assigned, not `os.environ[...]`.
            Value::Unknown if matches!(&*subscript.value, Expr::Name(name) if self.env.contains_key(name.id.as_str())) => {
                idioms::requirement_table_lookup(&self.env, &key).map_or(Value::Unknown, |entry| {
                    self.table_requirements.extend(entry.table);
                    Value::Str(entry.requirement)
                })
            },
            _ => Value::Unknown,
        }
    }

    fn eval_elements(&mut self, elts: &[Expr]) -> Vec<Value> {
        let mut values = Vec::with_capacity(elts.len());
        for elt in elts {
            if let Expr::Starred(starred) = elt {
                match self.eval(&starred.value) {
                    Value::List(spread, _) => values.extend(spread),
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
            .map_or(Value::Unknown, |items| Value::List(items, self.fresh()))
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
                let Value::List(items, _) = self.eval(&generator.iter) else {
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
            && matches!(arg, Value::List(..) | Value::RequirementsFile(_))
        {
            let mut value = arg.clone();
            if let Value::List(_, identity) = &mut value {
                *identity = if name == "tuple" {
                    Identity(None)
                } else {
                    self.fresh()
                };
            }
            return value;
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
                Value::List(positional.collect(), Identity(None)),
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
}

/// `dict[key] = value`: a repeated key keeps its first position and the last value.
fn insert_entry(items: &mut Vec<(String, Value)>, key: String, value: Value) {
    match items.iter_mut().find(|(existing, _)| *existing == key) {
        Some((_, slot)) => *slot = value,
        None => items.push((key, value)),
    }
}

const fn str_nodes(len: usize) -> usize {
    1 + len / STR_BYTES_PER_NODE
}

/// `left + right`; a list result is the object `identity`. A string over
/// the size cap is `Unknown` before it is built.
fn add(left: Value, right: Value, identity: Identity) -> Value {
    match (left, right) {
        (Value::Str(left), Value::Str(right)) => {
            if str_nodes(left.len().saturating_add(right.len())) > MAX_VALUE_NODES {
                return Value::Unknown;
            }
            Value::Str(left + &right)
        },
        (Value::List(mut left, _), Value::List(right, _)) => {
            left.extend(right);
            Value::List(left, identity)
        },
        (Value::List(mut items, _), other) | (other, Value::List(mut items, _))
            if !matches!(other, Value::Str(_)) =>
        {
            items.push(other);
            Value::List(items, identity)
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
    //! `CPython`). The model keeps lists and dicts on a heap, so a name, a
    //! dict entry, or a helper's result that aliases one sees `+=`, item
    //! assignment, and `del` made through another (#565). Lists hold only
    //! strings and dicts only lists, so no object can contain itself.

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

        fn dict_expr(&mut self, depth: usize) -> Ex {
            let choice = if depth >= 3 { 0 } else { self.tape.pick(4) };
            let base = match choice {
                1 => self.var(Kind::Dict).map(|name| Box::new(Ex::Var(name))),
                2 => return Ex::DictComp(Box::new(self.list_expr(depth + 1))),
                3 => match self.var(Kind::Dict) {
                    Some(name) => return Ex::Var(name),
                    None => None,
                },
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

        fn alias(&mut self) -> St {
            let kind = self.tape.choose(&[Kind::List, Kind::Dict]);
            let Some(old) = self.var(kind) else {
                return self.assign_new();
            };
            let name = self.fresh("v");
            self.scope.vars.push((name.clone(), kind));
            St::Assign(name, Ex::Var(old))
        }

        fn stmt(&mut self) -> St {
            match self.tape.pick(8) {
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
                6 => match self.var(Kind::List) {
                    Some(name) => St::AugAdd(name, self.list_expr(1)),
                    None => self.assign_new(),
                },
                7 => self.alias(),
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

    /// A model value: strings inline, lists and dicts as heap objects.
    #[derive(Debug, Clone)]
    enum Ref {
        Str(String),
        Obj(usize),
    }

    #[derive(Debug)]
    enum Obj {
        List(Vec<Ref>),
        Dict(Vec<(String, Ref)>),
    }

    fn py_insert(items: &mut Vec<(String, Ref)>, key: &str, value: Ref) {
        match items.iter_mut().find(|(existing, _)| existing == key) {
            Some((_, slot)) => *slot = value,
            None => items.push((key.to_owned(), value)),
        }
    }

    /// What `CPython` computes; `None` where it raises (`KeyError`).
    #[derive(Default)]
    struct Model {
        globals: HashMap<String, Ref>,
        funcs: HashMap<String, (Vec<String>, Ex)>,
        heap: Vec<Obj>,
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
                    let value = self.eval_global(value)?;
                    self.globals.insert(name.clone(), value);
                },
                St::AugAdd(name, value) => {
                    let target = self.globals.get(name)?.clone();
                    let value = self.eval_global(value)?;
                    match target {
                        Ref::Str(left) => {
                            let Ref::Str(right) = value else {
                                return None;
                            };
                            self.globals.insert(name.clone(), Ref::Str(left + &right));
                        },
                        // `list += ...` extends the object in place.
                        Ref::Obj(id) => {
                            let items = self.list(&value)?;
                            let Obj::List(target) = &mut self.heap[id] else {
                                return None;
                            };
                            target.extend(items);
                        },
                    }
                },
                St::SetItem(name, key, value) => {
                    let value = self.eval_global(value)?;
                    py_insert(self.dict_mut(name)?, key, value);
                },
                St::Del(name, key) => {
                    let items = self.dict_mut(name)?;
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

        fn alloc(&mut self, obj: Obj) -> Ref {
            self.heap.push(obj);
            Ref::Obj(self.heap.len() - 1)
        }

        fn list(&self, value: &Ref) -> Option<Vec<Ref>> {
            match value {
                Ref::Obj(id) => match &self.heap[*id] {
                    Obj::List(items) => Some(items.clone()),
                    Obj::Dict(_) => None,
                },
                Ref::Str(_) => None,
            }
        }

        fn dict_mut(&mut self, name: &str) -> Option<&mut Vec<(String, Ref)>> {
            let id = match self.globals.get(name)? {
                Ref::Obj(id) => *id,
                Ref::Str(_) => return None,
            };
            match &mut self.heap[id] {
                Obj::Dict(items) => Some(items),
                Obj::List(_) => None,
            }
        }

        fn resolve(&self, value: &Ref) -> Py {
            match value {
                Ref::Str(text) => Py::Str(text.clone()),
                Ref::Obj(id) => match &self.heap[*id] {
                    Obj::List(items) => {
                        Py::List(items.iter().map(|item| self.resolve(item)).collect())
                    },
                    Obj::Dict(items) => Py::Dict(
                        items
                            .iter()
                            .map(|(key, value)| (key.clone(), self.resolve(value)))
                            .collect(),
                    ),
                },
            }
        }

        fn eval_global(&mut self, expr: &Ex) -> Option<Ref> {
            let env = self.globals.clone();
            self.eval(expr, &env)
        }

        fn value(&mut self, expr: &Ex) -> Option<Py> {
            let value = self.eval_global(expr)?;
            Some(self.resolve(&value))
        }

        fn strings(&mut self, iter: &Ex, env: &HashMap<String, Ref>) -> Option<Vec<String>> {
            let value = self.eval(iter, env)?;
            self.list(&value)?
                .into_iter()
                .map(|item| match item {
                    Ref::Str(text) => Some(text),
                    Ref::Obj(_) => None,
                })
                .collect()
        }

        fn eval(&mut self, expr: &Ex, env: &HashMap<String, Ref>) -> Option<Ref> {
            Some(match expr {
                Ex::Lit(text) => Ref::Str((*text).to_owned()),
                Ex::Var(name) => env.get(name)?.clone(),
                Ex::Add(left, right) => match (self.eval(left, env)?, self.eval(right, env)?) {
                    (Ref::Str(left), Ref::Str(right)) => Ref::Str(left + &right),
                    (left, right) => {
                        let mut items = self.list(&left)?;
                        items.extend(self.list(&right)?);
                        self.alloc(Obj::List(items))
                    },
                },
                Ex::List(spread, items) => {
                    let mut values = match spread {
                        Some(spread) => {
                            let spread = self.eval(spread, env)?;
                            self.list(&spread)?
                        },
                        None => Vec::new(),
                    };
                    for item in items {
                        values.push(self.eval(item, env)?);
                    }
                    self.alloc(Obj::List(values))
                },
                Ex::Comp(iter, suffix) => {
                    let items = self
                        .strings(iter, env)?
                        .into_iter()
                        .map(|text| Ref::Str(text + suffix))
                        .collect();
                    self.alloc(Obj::List(items))
                },
                Ex::Copy(inner) => {
                    let inner = self.eval(inner, env)?;
                    let items = self.list(&inner)?;
                    self.alloc(Obj::List(items))
                },
                Ex::Dict(base, pairs) => self.eval_dict(base.as_deref(), pairs, env)?,
                Ex::DictComp(iter) => {
                    let mut items = Vec::new();
                    for key in self.strings(iter, env)? {
                        let value = self.alloc(Obj::List(vec![Ref::Str(key.clone())]));
                        py_insert(&mut items, &key, value);
                    }
                    self.alloc(Obj::Dict(items))
                },
                Ex::Sub(base, key) => {
                    let Ref::Obj(id) = self.eval(base, env)? else {
                        return None;
                    };
                    let Obj::Dict(items) = &self.heap[id] else {
                        return None;
                    };
                    items
                        .iter()
                        .find(|(existing, _)| existing == key)?
                        .1
                        .clone()
                },
                Ex::Call(name, args) => {
                    let (params, body) = self.funcs.get(name)?.clone();
                    // A helper sees module globals and its own parameters.
                    let mut scope = self.globals.clone();
                    for (param, arg) in params.iter().zip(args) {
                        let arg = self.eval(arg, env)?;
                        scope.insert(param.clone(), arg);
                    }
                    self.eval(&body, &scope)?
                },
            })
        }

        fn eval_dict(
            &mut self,
            base: Option<&Ex>,
            pairs: &[(&'static str, Ex)],
            env: &HashMap<String, Ref>,
        ) -> Option<Ref> {
            let mut items = match base {
                Some(base) => match self.eval(base, env)? {
                    Ref::Obj(id) => match &self.heap[id] {
                        Obj::Dict(items) => items.clone(),
                        Obj::List(_) => return None,
                    },
                    Ref::Str(_) => return None,
                },
                None => Vec::new(),
            };
            for (key, value) in pairs {
                let value = self.eval(value, env)?;
                py_insert(&mut items, key, value);
            }
            Some(self.alloc(Obj::Dict(items)))
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
            Value::List(items, _) => items
                .iter()
                .map(|item| known(Some(item)))
                .collect::<Option<_>>()
                .map(Py::List),
            Value::Dict { partial: true, .. } | Value::RequirementsFile(_) | Value::Unknown => None,
            Value::Dict { items, .. } => items
                .iter()
                .map(|(key, value)| Some((key.clone(), known(Some(value))?)))
                .collect::<Option<_>>()
                .map(Py::Dict),
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
        let mut model = Model::run(stmts)?;
        let mut install_strings = Vec::new();
        py_strings(&model.value(install)?, &mut install_strings);
        let Py::Dict(items) = model.value(extras)? else {
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

    const MUTATIONS: [&str; 6] = [
        ".append('z')",
        ".extend(['z'])",
        ".insert(0, 'z')",
        ".clear()",
        ".remove('a')",
        "[0:0] = ['z']",
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
        #![proptest_config(ProptestConfig::with_cases(crate::internals::proptest_cases(256)))]

        #[test]
        fn evaluation_matches_python_model(
            tape in prop::collection::vec(any::<u8>(), 0..160),
            style in any::<u8>(),
        ) {
            let mut builder = Builder::new(&tape);
            let stmts = builder.stmts();
            let install = builder.list_expr(0);
            let extras = builder.dict_expr(0);
            let Some(mut model) = Model::run(&stmts) else {
                return Ok(());
            };
            let (Some(want_install), Some(want_extras)) = (model.value(&install), model.value(&extras)) else {
                return Ok(());
            };
            let source = Style(style).program(&stmts, &install, &extras);
            let call = evaluate(&source).expect("generated setup.py has a setup() call");
            prop_assert_eq!(known(call.keyword("install_requires")), Some(want_install), "{}", source);
            prop_assert_eq!(known(call.keyword("extras_require")), Some(want_extras), "{}", source);
        }

        #[test]
        fn changes_through_one_name_reach_its_aliases(
            tape in prop::collection::vec(any::<u8>(), 0..160),
            through_alias in any::<bool>(),
            style in any::<u8>(),
        ) {
            let mut builder = Builder::new(&tape);
            let mut stmts = builder.stmts();
            let list = builder.fresh("v");
            let list_alias = builder.fresh("v");
            let dict = builder.fresh("v");
            let dict_alias = builder.fresh("v");
            stmts.push(St::Assign(list.clone(), builder.list_expr(1)));
            stmts.push(St::Assign(dict.clone(), builder.dict_expr(1)));
            stmts.push(St::Assign(list_alias.clone(), Ex::Var(list.clone())));
            stmts.push(St::Assign(dict_alias.clone(), Ex::Var(dict.clone())));
            let (changed, read) = if through_alias {
                ((&list_alias, &dict_alias), (&list, &dict))
            } else {
                ((&list, &dict), (&list_alias, &dict_alias))
            };
            stmts.push(St::AugAdd(changed.0.clone(), builder.list_expr(1)));
            stmts.push(St::SetItem(changed.1.clone(), builder.tape.choose(&KEYS), Ex::Var(read.0.clone())));
            if builder.tape.pick(2) == 1 {
                stmts.push(St::Del(changed.1.clone(), builder.tape.choose(&KEYS)));
            }
            let install = Ex::Var(read.0.clone());
            let extras = Ex::Var(read.1.clone());
            let Some(mut model) = Model::run(&stmts) else {
                return Ok(());
            };
            let (Some(want_install), Some(want_extras)) = (model.value(&install), model.value(&extras)) else {
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
            let Some(Value::Dict { items: got_extras, .. }) = call.keyword("extras_require") else {
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
                    let Some(Py::Dict(items)) = model.globals.get(&dict).map(|value| model.resolve(value)) else {
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
    fn nodes_counts_every_container_and_leaf() {
        let leaf = || Value::Str("a".to_owned());
        assert_eq!(Value::List(vec![leaf(), leaf()], Identity(None)).nodes(), 3);
        let dict = Value::Dict {
            items: vec![("k".to_owned(), leaf())],
            partial: false,
            id: Identity(None),
        };
        assert_eq!(dict.nodes(), 2);
    }

    /// setuptools reads a string `install_requires` with `parse_strings`
    /// (#965): ` #` starts a comment, a `\`-ending line takes the next one
    /// with no space between, and list items go through the same reader.
    #[test]
    fn dependency_items_read_strings_like_setuptools_parse_strings() {
        let texts = |value: Value| requirement_texts(Some(&value)).expect("complete");
        assert_eq!(
            texts(Value::Str(
                "\nrequests \\\n  >=2\nrich  # pretty output\nrich#x\n# note\n".to_owned()
            )),
            ["requests>=2", "rich", "rich#x"]
        );
        assert_eq!(
            texts(Value::List(
                vec![
                    Value::Str("requests \\".to_owned()),
                    Value::Str(">=2".to_owned()),
                    Value::Str("a\nb".to_owned()),
                ],
                Identity(None)
            )),
            ["requests>=2", "a", "b"]
        );
        // setuptools drops an item whose `\` has nothing to join; chokkin
        // keeps it as written so it is reported instead (ADR 0005).
        assert_eq!(
            texts(Value::Str("requests \\\n".to_owned())),
            ["requests \\"]
        );
    }

    /// An item chokkin cannot read stands for text it has not seen, so a
    /// `\` before it joins nothing across it.
    #[test]
    fn unreadable_item_ends_the_continuation_run() {
        let items = dependency_items(&Value::List(
            vec![
                Value::Str("foo \\".to_owned()),
                Value::Unknown,
                Value::Str("bar".to_owned()),
            ],
            Identity(None),
        ));
        assert!(!items.complete);
        let texts: Vec<&str> = items
            .items
            .iter()
            .filter_map(|item| match item {
                DependencyItem::Requirement(text) => Some(text.as_str()),
                DependencyItem::RequirementsFile(_) => None,
            })
            .collect();
        assert_eq!(texts, ["foo \\", "bar"]);
    }

    #[test]
    fn spreading_a_partial_dict_keeps_it_partial() {
        let call = evaluate(
            "base = {'a': ['x'], **opaque()}
setup(extras_require={**base, 'b': ['y']})
",
        )
        .expect("setup() call");
        let Some(Value::Dict { items, partial, .. }) = call.keyword("extras_require") else {
            panic!("extras_require is not a dict");
        };
        assert!(partial);
        let keys: Vec<&str> = items.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["a", "b"]);
    }

    #[test]
    fn missing_key_of_a_known_dict_is_not_looked_up_in_the_tables() {
        let call = evaluate(
            "reqs = ['requests>=2']
table = {'other': 'y'}
\
             setup(install_requires=[table['requests']])
",
        )
        .expect("setup() call");
        assert_eq!(
            call.keyword("install_requires"),
            Some(&Value::List(vec![Value::Unknown], Identity(None)))
        );
    }

    #[test]
    fn missing_key_of_a_partial_dict_is_looked_up_in_the_tables() {
        let call = evaluate(
            "reqs = ['requests>=2']
table = {'other': 'y', **opaque()}
\
             setup(install_requires=[table['requests']])
",
        )
        .expect("setup() call");
        assert_eq!(
            call.keyword("install_requires"),
            Some(&Value::List(
                vec![Value::Str("requests>=2".to_owned())],
                Identity(None)
            ))
        );
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
    fn assignment_under_an_unknown_key_keeps_the_known_entries() {
        let call = evaluate(
            "extras = {'s3': ['boto3']}\nkey = compute()\nextras[key] = ['gcs']\n\
             setup(extras_require=extras)\n",
        )
        .expect("setup() call");
        assert_eq!(
            call.keyword("extras_require"),
            Some(&Value::Dict {
                items: vec![(
                    "s3".to_owned(),
                    Value::List(vec![Value::Str("boto3".to_owned())], Identity(None))
                )],
                partial: true,
                id: Identity(None),
            })
        );
    }

    #[test]
    fn literal_index_assignment_and_del_replace_the_element() {
        // #868: a literal index in range names one element, so the old one
        // is no longer declared.
        for (source, expected) in [
            ("reqs = ['a']\nreqs[0] = 'b'\n", strings(&["b"])),
            ("reqs = ['a', 'b']\nreqs[-1] = 'c'\n", strings(&["a", "c"])),
            ("reqs = ['a', 'b']\ndel reqs[0]\n", strings(&["b"])),
            ("reqs = ['a', 'b']\ndel reqs[-2]\n", strings(&["b"])),
            ("reqs = ['a']\nalias = reqs\ndel alias[0]\n", strings(&[])),
            (
                "reqs = ['a']\nreqs += ['b']\nreqs[1] = 'c'\n",
                strings(&["a", "c"]),
            ),
            (
                "reqs = ['a', 'b']\nif C:\n    alias = reqs\ndel reqs[0]\nreqs += ['c']\n",
                strings(&["b", "c"]),
            ),
            (
                "reqs = ['a', 'b']\nreqs[0] = ['c', 'd']\n",
                Py::List(vec![strings(&["c", "d"]), Py::Str("b".to_owned())]),
            ),
        ] {
            let call =
                evaluate(&format!("{source}setup(install_requires=reqs)\n")).expect("setup() call");
            assert_eq!(
                known(call.keyword("install_requires")),
                Some(expected),
                "{source}"
            );
        }
    }

    #[test]
    fn other_index_assignment_and_del_mark_the_list_partial() {
        // An index we cannot read, one out of range (CPython raises), or one
        // counted over a spread of unknown length may name any element.
        for source in [
            "reqs = ['a']\nreqs[i] = 'b'\n",
            "reqs = ['a']\nreqs[1] = 'b'\n",
            "reqs = ['a']\ndel reqs[-2]\n",
            "reqs = ['a']\nreqs[-0] = 'b'\n",
            "reqs = ['a']\nreqs[0:1] = ['b']\n",
            "reqs = [*more, 'a']\nreqs[1] = 'b'\n",
            "reqs = ['a', *more]\ndel reqs[-1]\n",
            "reqs = ['a', 'b']\nreqs[True] = 'c'\n",
        ] {
            let call =
                evaluate(&format!("{source}setup(install_requires=reqs)\n")).expect("setup() call");
            let value = call.keyword("install_requires").expect("install_requires");
            let items = dependency_items(value);
            assert!(!items.complete, "{source}");
            assert!(
                items
                    .items
                    .contains(&DependencyItem::Requirement("a".to_owned())),
                "{source}"
            );
        }
    }

    #[test]
    fn index_into_a_list_of_unknown_order_keeps_every_element() {
        // A change we cannot follow, or a branch merge, keeps the elements
        // but not their order, so an index may name any of them.
        for (source, kept) in [
            ("reqs = ['a', 'b']\nreqs.reverse()\nreqs[0] = 'c'\n", "a"),
            (
                "reqs = ['a', 'b']\nreqs.insert(0, 'z')\nreqs[1] = 'c'\n",
                "b",
            ),
            (
                "reqs = ['a', 'b']\nreqs.sort(reverse=True)\ndel reqs[0]\n",
                "a",
            ),
            (
                "reqs = ['a', 'b']\nfor x in X:\n    reqs.insert(0, x)\ndel reqs[0]\n",
                "a",
            ),
            (
                "if C:\n    reqs = ['a', 'b']\nelse:\n    reqs = ['b', 'a']\ndel reqs[0]\n",
                "a",
            ),
            (
                "reqs = ['a', 'b']\nif C:\n    del reqs[0]\ndel reqs[-1]\n",
                "a",
            ),
            ("reqs = ['a']\nreqs += more\nreqs[-1] = 'c'\n", "a"),
            ("reqs = sorted(['b', 'a'])\ndel reqs[0]\n", "a"),
        ] {
            let call =
                evaluate(&format!("{source}setup(install_requires=reqs)\n")).expect("setup() call");
            let value = call.keyword("install_requires").expect("install_requires");
            let items = dependency_items(value);
            assert!(!items.complete, "{source}");
            assert!(
                items
                    .items
                    .contains(&DependencyItem::Requirement(kept.to_owned())),
                "{source}"
            );
        }
    }

    #[test]
    fn assignment_at_an_unread_index_declares_the_value() {
        // Whichever element `reqs[i]` names, the value is in the list.
        for source in [
            "reqs = ['a']\nreqs[i] = 'b'\n",
            "reqs = ['a']\nreqs.reverse()\nreqs[0] = 'b'\n",
        ] {
            let call =
                evaluate(&format!("{source}setup(install_requires=reqs)\n")).expect("setup() call");
            let value = call.keyword("install_requires").expect("install_requires");
            let items = dependency_items(value);
            assert!(!items.complete, "{source}");
            assert!(
                items
                    .items
                    .contains(&DependencyItem::Requirement("b".to_owned())),
                "{source}"
            );
        }
        let call = evaluate("reqs = ['a']\nreqs[0:1] = 'ab'\nsetup(install_requires=reqs)\n")
            .expect("setup() call");
        let value = call.keyword("install_requires").expect("install_requires");
        assert!(
            !dependency_items(value)
                .items
                .contains(&DependencyItem::Requirement("ab".to_owned()))
        );
    }

    #[test]
    fn nested_subscript_assignment_follows_string_keys() {
        let call = evaluate(
            "extras = {'dev': {'lint': ['ruff']}, 'test': ['pytest']}\n\
             extras['dev']['type'] = ['mypy']\n\
             extras['test'][0] = 'nose'\n\
             setup(extras_require=extras)\n",
        )
        .expect("setup() call");
        assert_eq!(
            known(call.keyword("extras_require")),
            Some(Py::Dict(vec![
                (
                    "dev".to_owned(),
                    Py::Dict(vec![
                        ("lint".to_owned(), strings(&["ruff"])),
                        ("type".to_owned(), strings(&["mypy"])),
                    ])
                ),
                ("test".to_owned(), strings(&["nose"])),
            ]))
        );
    }

    #[test]
    fn del_by_index_removes_only_that_element() {
        let call = evaluate(
            "extras = {'dev': ['ruff', 'mypy'], 'test': ['pytest']}\ndel extras['dev'][0]\n\
             setup(extras_require=extras)\n",
        )
        .expect("setup() call");
        assert_eq!(
            known(call.keyword("extras_require")),
            Some(Py::Dict(vec![
                ("dev".to_owned(), strings(&["mypy"])),
                ("test".to_owned(), strings(&["pytest"])),
            ]))
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

    fn strings(items: &[&str]) -> Py {
        Py::List(
            items
                .iter()
                .map(|item| Py::Str((*item).to_owned()))
                .collect(),
        )
    }

    fn install_requires(source: &str) -> Option<Py> {
        known(
            evaluate(source)
                .expect("setup() call")
                .keyword("install_requires"),
        )
    }

    #[test]
    fn augmented_add_reaches_every_alias_of_a_list() {
        assert_eq!(
            install_requires(
                "base = ['a']\nreqs = base\nbase += ['b']\nsetup(install_requires=reqs)\n"
            ),
            Some(strings(&["a", "b"]))
        );
        assert_eq!(
            install_requires(
                "base = ['a']\nreqs = base\nreqs += ['b']\nsetup(install_requires=base)\n"
            ),
            Some(strings(&["a", "b"]))
        );
    }

    #[test]
    fn unfollowed_mutation_marks_every_alias_partial() {
        let call =
            evaluate("base = ['a']\nreqs = base\nbase.append('b')\nsetup(install_requires=reqs)\n")
                .expect("setup() call");
        let value = call.keyword("install_requires").expect("install_requires");
        assert!(!dependency_items(value).complete);
    }

    #[test]
    fn a_list_held_by_a_dict_sees_later_changes() {
        let call = evaluate(
            "base = ['a']\nextras = {'t': base}\nbase += ['b']\nextras['t'] += ['c']\n\
             setup(install_requires=base, extras_require=extras)\n",
        )
        .expect("setup() call");
        assert_eq!(
            known(call.keyword("install_requires")),
            Some(strings(&["a", "b", "c"]))
        );
        assert_eq!(
            known(call.keyword("extras_require")),
            Some(Py::Dict(vec![("t".to_owned(), strings(&["a", "b", "c"]))]))
        );
    }

    #[test]
    fn dict_changes_reach_every_alias() {
        let call = evaluate(
            "extras = {'a': ['x']}\nalias = extras\nextras['b'] = ['y']\ndel extras['a']\n\
             outer = {'inner': alias}\nouter['inner']['c'] = ['z']\nsetup(extras_require=alias)\n",
        )
        .expect("setup() call");
        assert_eq!(
            known(call.keyword("extras_require")),
            Some(Py::Dict(vec![
                ("b".to_owned(), strings(&["y"])),
                ("c".to_owned(), strings(&["z"])),
            ]))
        );
    }

    #[test]
    fn copies_and_tuples_do_not_share_changes() {
        for copy in [
            "list(base)",
            "sorted(base)",
            "base + []",
            "[*base]",
            "[x for x in base]",
        ] {
            assert_eq!(
                install_requires(&format!(
                    "base = ['a']\nreqs = {copy}\nbase += ['b']\nsetup(install_requires=reqs)\n"
                )),
                Some(strings(&["a"])),
                "{copy}"
            );
        }
        assert_eq!(
            install_requires(
                "base = ('a',)\nreqs = base\nbase += ('b',)\nsetup(install_requires=reqs)\n"
            ),
            Some(strings(&["a"]))
        );
    }

    #[test]
    fn rebinding_in_a_loop_leaves_aliases_complete() {
        let call = evaluate(
            "install_requires = ['requests']\nreqs = install_requires\nfor line in lines:\n    \
             reqs = reqs + [line]\nreqs += ['b']\nsetup(install_requires=install_requires)\n",
        )
        .expect("setup() call");
        let items = dependency_items(call.keyword("install_requires").expect("install_requires"));
        assert_eq!(
            items,
            DependencyItems {
                items: vec![DependencyItem::Requirement("requests".to_owned())],
                complete: true,
            }
        );
    }

    #[test]
    fn change_after_branches_keeps_the_other_branch_entries() {
        let call = evaluate(
            "requirements = ['requests']\ninstall_requires = requirements\nif win:\n    \
             install_requires = requirements + ['pywin32']\nrequirements += ['click']\n\
             setup(install_requires=install_requires)\n",
        )
        .expect("setup() call");
        let items = dependency_items(call.keyword("install_requires").expect("install_requires"));
        assert!(items.complete);
        for name in ["requests", "pywin32"] {
            assert!(
                items
                    .items
                    .contains(&DependencyItem::Requirement(name.to_owned())),
                "{name} missing from {items:?}"
            );
        }
    }

    #[test]
    fn names_aliased_in_every_branch_stay_aliased() {
        let call = evaluate(
            "if win:\n    a = b = ['x']\nelse:\n    a = b = ['y']\nb += ['z']\n\
             setup(install_requires=a)\n",
        )
        .expect("setup() call");
        let items = dependency_items(call.keyword("install_requires").expect("install_requires"));
        assert!(
            items
                .items
                .contains(&DependencyItem::Requirement("z".to_owned()))
        );
    }

    #[test]
    fn self_reference_is_unknown_not_unbounded() {
        let call = evaluate(
            "base = ['a']\nbase += [base]\nextras = {}\nextras['self'] = extras\n\
             setup(install_requires=base, extras_require=extras)\n",
        )
        .expect("setup() call");
        let install = dependency_items(call.keyword("install_requires").expect("install_requires"));
        assert!(!install.complete);
        assert_eq!(
            install.items,
            vec![DependencyItem::Requirement("a".to_owned())]
        );
        assert_eq!(known(call.keyword("extras_require")), None);
    }

    #[test]
    fn annotated_assignments_bind_and_loops_taint_them() {
        assert_eq!(
            install_requires("reqs: list = ['a']\nsetup(install_requires=reqs)\n"),
            Some(strings(&["a"]))
        );
        assert_eq!(
            install_requires(
                "reqs = ['a']\nfor x in y:\n    reqs: list = reqs + [x]\n\
                 setup(install_requires=reqs)\n"
            ),
            None
        );
    }

    #[test]
    fn loops_taint_every_value_they_may_change() {
        for source in [
            "reqs = ['a']\nfor reqs, x in pairs:\n    pass\nsetup(install_requires=reqs)\n",
            "base = ['a']\nreqs = base\nfor x in y:\n    reqs += [x]\n\
             setup(install_requires=base)\n",
            "reqs = ['a']\nfor a in b:\n    for c in d:\n        reqs.append(c)\n\
             setup(install_requires=reqs)\n",
        ] {
            assert_eq!(install_requires(source), None, "{source}");
        }
    }

    #[test]
    fn unpacking_binds_only_matching_lengths() {
        assert_eq!(
            install_requires("reqs, extra = (['a'], ['b'])\nsetup(install_requires=reqs)\n"),
            Some(strings(&["a"]))
        );
        assert_eq!(
            install_requires("reqs, *rest = (['a'], ['b'], ['c'])\nsetup(install_requires=reqs)\n"),
            None
        );
    }

    #[test]
    fn set_difference_is_not_read_as_a_union() {
        for source in [
            "reqs = {'a', 'b'} - {'b'}\nsetup(install_requires=reqs)\n",
            "reqs = {'a', 'b'}\nreqs -= {'b'}\nsetup(install_requires=reqs)\n",
        ] {
            assert_eq!(install_requires(source), None, "{source}");
        }
    }

    #[test]
    fn equal_strings_survive_a_branch_merge() {
        assert_eq!(
            install_requires(
                "reqs = 'a'\nif win:\n    other = ['b']\nsetup(install_requires=reqs)\n"
            ),
            Some(Py::Str("a".to_owned()))
        );
    }

    #[test]
    fn names_rebound_in_a_branch_stop_sharing_their_old_object() {
        assert_eq!(
            install_requires(
                "base = ['a']\ninst = base\nother = base\nif win:\n    inst = ['x']\n    \
                 other = ['y']\nother += ['z']\nsetup(install_requires=inst)\n"
            ),
            Some(strings(&["a", "x"]))
        );
    }

    fn big_list(len: usize) -> String {
        format!("[{}]", vec!["'x'"; len].join(", "))
    }

    /// `+=` charges the new value once per copy: four copies of 25,000
    /// nodes fit `MAX_VALUE_NODES`, one more item each does not.
    #[test]
    fn in_place_change_is_capped_by_its_copies() {
        for holder in ["{'k1': base, 'k2': base, 'k3': base}", "[base, base, base]"] {
            for (len, fits) in [(24_998, true), (24_999, false)] {
                let source = format!(
                    "base = {}\nholder = {holder}\nbase += ['y']\nsetup(install_requires=base)\n",
                    big_list(len)
                );
                assert_eq!(
                    install_requires(&source).is_some(),
                    fits,
                    "{holder} with {len} items"
                );
            }
        }
    }

    #[test]
    fn dict_size_counts_the_dict_itself() {
        for (len, fits) in [(49_998, true), (49_999, false)] {
            let list = big_list(len);
            let call = evaluate(&format!(
                "extras = {{'a': {list}, 'b': {list}}}\nsetup(extras_require=extras)\n"
            ))
            .expect("setup() call");
            assert_eq!(
                known(call.keyword("extras_require")).is_some(),
                fits,
                "{len} items"
            );
        }
    }

    #[test]
    fn method_call_on_a_missing_entry_marks_its_dict_partial() {
        let partial = |source: &str| match evaluate(source)
            .expect("setup() call")
            .keyword("extras_require")
        {
            Some(Value::Dict { partial, .. }) => *partial,
            other => panic!("{other:?}"),
        };
        assert!(partial(
            "extras = {'a': ['x']}\nextras['b'].append('y')\nsetup(extras_require=extras)\n"
        ));
        assert!(!partial(
            "extras = {'a': ['x']}\nextras['a'].append('y')\nsetup(extras_require=extras)\n"
        ));
    }

    /// Once the step budget is spent the result is `Unknown` anyway, so a
    /// later in-place change must not walk every stored value: thousands of
    /// them over a large environment would run for minutes.
    #[test]
    fn in_place_changes_after_the_budget_is_spent_do_no_work() {
        let mut copies = String::new();
        for i in 0..40 {
            writeln!(copies, "v{i} = base").expect("write to String");
        }
        let source = format!(
            "base = {}\n{copies}x = ['a']\n{}{}setup(install_requires=base)\n",
            big_list(90_000),
            "x = x + x\n".repeat(18),
            "base.append('y')\n".repeat(2_000),
        );
        assert_eq!(install_requires(&source), None);
    }

    /// `exhausted` holds exactly when `spend` has refused, so a guard on it
    /// never skips work the budget still allows.
    #[test]
    fn budget_is_exhausted_exactly_when_spend_refuses() {
        let root = Path::new(".");
        let mut evaluator = Evaluator {
            root,
            env: HashMap::new(),
            functions: HashMap::new(),
            depth: 0,
            steps: 0,
            objects: 0,
            probed_missing: Vec::new(),
            table_requirements: BTreeSet::new(),
            ordered: HashSet::new(),
        };
        assert!(evaluator.spend(MAX_STEPS));
        assert!(!evaluator.exhausted());
        assert!(!evaluator.spend(1));
        assert!(evaluator.exhausted());
    }

    /// A string is charged by length, so doubling one ends as `Unknown`
    /// at the size cap instead of allocating until the process aborts.
    #[test]
    fn string_doubling_is_capped_by_length() {
        let doubled = |times: usize| {
            format!(
                "s = '{}'\n{}setup(install_requires=[s])\n",
                "x".repeat(1024),
                "s = s + s\n".repeat(times)
            )
        };
        // 2 MiB fits; 4 MiB is over 100,000 nodes of 32 bytes.
        assert!(install_requires(&doubled(11)).is_some());
        assert_eq!(install_requires(&doubled(12)), None);
        assert_eq!(install_requires(&doubled(40)), None);
        let augmented = format!(
            "s = '{}'\n{}setup(install_requires=[s])\n",
            "x".repeat(1024),
            "s += s\n".repeat(40)
        );
        assert_eq!(install_requires(&augmented), None);
    }

    /// Many copies of one long string count every copy's length.
    #[test]
    fn list_of_long_strings_is_capped_by_total_length() {
        let source = |copies: usize| {
            format!(
                "s = '{}'\nreqs = [{}]\nsetup(install_requires=reqs)\n",
                "x".repeat(128 * 1024),
                vec!["s"; copies].join(", ")
            )
        };
        // 4,097 nodes per string and 1 for the list.
        assert!(install_requires(&source(24)).is_some());
        assert_eq!(install_requires(&source(25)), None);
    }

    #[test]
    fn nodes_count_string_and_key_lengths() {
        assert_eq!(Value::Str("x".repeat(31)).nodes(), 1);
        assert_eq!(Value::Str("x".repeat(32)).nodes(), 2);
        let dict = Value::Dict {
            items: vec![("k".repeat(64), Value::Str("a".to_owned()))],
            partial: false,
            id: Identity(None),
        };
        assert_eq!(dict.nodes(), 4);
    }

    /// Each branch copies the environment, so nested `if`s over a large
    /// one are charged per copy: 20 levels of three copies of 70 strings
    /// of 2,049 nodes spend the budget, and the rest stop copying.
    #[test]
    fn nested_branches_are_charged_for_copying_the_environment() {
        let mut source = format!("s = '{}'\n", "x".repeat(64 * 1024));
        for i in 0..70 {
            writeln!(source, "v{i} = s").expect("write to String");
        }
        let mut indent = String::new();
        for _ in 0..20 {
            writeln!(source, "{indent}if c:").expect("write to String");
            indent.push_str("    ");
        }
        writeln!(source, "{indent}setup(install_requires=['a'])").expect("write to String");
        assert_eq!(install_requires(&source), None);
    }

    /// `+` builds a string exactly at the cap and refuses one byte-node more.
    #[test]
    fn string_concatenation_is_refused_just_over_the_cap() {
        let at_cap = (MAX_VALUE_NODES - 1) * STR_BYTES_PER_NODE;
        let concat = |len: usize| {
            add(
                Value::Str("x".repeat(len - 1)),
                Value::Str("x".to_owned()),
                Identity(None),
            )
        };
        assert!(matches!(concat(at_cap), Value::Str(_)));
        assert_eq!(concat(at_cap + STR_BYTES_PER_NODE), Value::Unknown);
    }

    /// An `if` without `else` copies the environment three times: the saved
    /// state, the fallthrough, and the branch.
    #[test]
    fn branches_charge_one_copy_each_plus_the_saved_state() {
        let mut evaluator = Evaluator {
            root: Path::new("."),
            env: HashMap::from([("s".to_owned(), Value::Str("x".repeat(320)))]),
            functions: HashMap::new(),
            depth: 0,
            steps: 0,
            objects: 0,
            probed_missing: Vec::new(),
            table_requirements: BTreeSet::new(),
            ordered: HashSet::new(),
        };
        let empty: &[Stmt] = &[];
        assert!(evaluator.exec_branches(&[empty], true).is_none());
        assert_eq!(evaluator.steps, 3 * 11);
    }
}
