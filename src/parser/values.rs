//! What an expression may evaluate to, as far as module objects go (#1427):
//! the module a function returns, a container holds, a class or instance
//! attribute keeps, or a `for` / `match` binds. A may analysis: every value
//! a flow can carry is kept, and a flow it does not follow carries nothing,
//! so a module read through it is not a reference.

use std::cell::Cell;

use ruff_python_ast::{
    Comprehension, Expr, ExprCall, ExprLambda, ExprNumberLiteral, Number, Parameters, Pattern,
    Stmt, StmtFunctionDef, UnaryOp,
};
use rustc_hash::FxHashMap;

use super::attributes::write_receiver;

/// How many values one expression keeps, and how deep calls are followed.
const MAX_VALUES: usize = 16;
const MAX_CALL_DEPTH: u32 = 4;
/// How many function bodies one top-level evaluation may follow.
const MAX_CALLS: u32 = 64;
/// How deep the evaluated expressions may nest: the walk itself goes to
/// [`super::depth::MAX_AST_DEPTH`] on a stack these frames would add to.
const MAX_NESTING: u32 = 64;

/// One value an expression may hold.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Value<'ast> {
    /// A dotted name as the module scope reads it (`pkg.m`); `get.()` is
    /// what calling `get`, a name bound outside this module, returns.
    Dotted(String),
    /// An instance of a module-level class.
    Instance(&'ast str),
    /// A method of a module-level class, read from an instance or not.
    Method(&'ast str, &'ast str, bool),
    Lambda(&'ast ExprLambda),
    /// A tuple or list display: what each element holds.
    Seq(Vec<Values<'ast>>),
    /// A sequence whose elements' positions are unknown (a comprehension, a
    /// display with a starred element): what any element may hold.
    Bag(Values<'ast>),
}

pub(super) type Values<'ast> = Vec<Value<'ast>>;

/// The dotted names among `values`.
pub(super) fn dotted<'v>(values: &'v [Value<'_>]) -> impl Iterator<Item = &'v str> {
    values.iter().filter_map(|value| match value {
        Value::Dotted(text) => Some(text.as_str()),
        _ => None,
    })
}

fn push<'ast>(out: &mut Values<'ast>, value: Value<'ast>) {
    if out.len() < MAX_VALUES && !out.contains(&value) {
        out.push(value);
    }
}

pub(super) fn extend<'ast>(out: &mut Values<'ast>, values: Values<'ast>) {
    for value in values {
        push(out, value);
    }
}

/// What iterating `values` yields: the elements of the displays among them.
pub(super) fn elements<'ast>(values: &[Value<'ast>]) -> Values<'ast> {
    let mut out = Vec::new();
    for value in values {
        match value {
            Value::Seq(elts) => {
                for elt in elts {
                    extend(&mut out, elt.clone());
                }
            },
            Value::Bag(elts) => extend(&mut out, elts.clone()),
            _ => {},
        }
    }
    out
}

/// Bind the names in an assignment target to what `values` hands each.
pub(super) fn destructure<'ast>(
    target: &'ast Expr,
    values: Values<'ast>,
    bind: &mut impl FnMut(&'ast str, Values<'ast>),
) {
    match target {
        Expr::Name(name) => bind(name.id.as_str(), values),
        Expr::Starred(starred) => destructure(&starred.value, values, bind),
        Expr::Tuple(tuple) => unpack(&tuple.elts, &values, bind),
        Expr::List(list) => unpack(&list.elts, &values, bind),
        _ => {},
    }
}

fn unpack<'ast>(
    targets: &'ast [Expr],
    values: &[Value<'ast>],
    bind: &mut impl FnMut(&'ast str, Values<'ast>),
) {
    let star = targets.iter().position(Expr::is_starred_expr);
    let mut parts: Vec<Values<'ast>> = vec![Vec::new(); targets.len()];
    for value in values {
        match value {
            Value::Seq(elts) => split_sequence(targets.len(), star, elts, &mut parts),
            Value::Bag(elts) => spread(star, elts, &mut parts),
            _ => {},
        }
    }
    for (target, part) in targets.iter().zip(parts) {
        destructure(target, part, bind);
    }
}

/// Hand the elements of a display to `count` targets, the one at `star`
/// taking the rest as a display of its own.
fn split_sequence<'ast>(
    count: usize,
    star: Option<usize>,
    elts: &[Values<'ast>],
    parts: &mut [Values<'ast>],
) {
    match star {
        None if elts.len() == count => {
            for (part, elt) in parts.iter_mut().zip(elts) {
                extend(part, elt.clone());
            }
        },
        Some(star) if elts.len() + 1 >= count => {
            let after = count - star - 1;
            let rest_end = elts.len() - after;
            for (index, part) in parts.iter_mut().enumerate() {
                match index.cmp(&star) {
                    std::cmp::Ordering::Less => extend(part, elts[index].clone()),
                    std::cmp::Ordering::Equal => {
                        push(part, Value::Seq(elts[star..rest_end].to_vec()));
                    },
                    std::cmp::Ordering::Greater => {
                        extend(part, elts[rest_end + index - star - 1].clone());
                    },
                }
            }
        },
        _ => {},
    }
}

/// Hand any element of a sequence of unknown positions to every target, the
/// one at `star` taking the rest.
fn spread<'ast>(star: Option<usize>, elts: &Values<'ast>, parts: &mut [Values<'ast>]) {
    for (index, part) in parts.iter_mut().enumerate() {
        if Some(index) == star {
            push(part, Value::Bag(elts.clone()));
        } else {
            extend(part, elts.clone());
        }
    }
}

/// Bind the capture names in a `case` pattern to what `values` hands each.
pub(super) fn bind_pattern<'ast>(
    pattern: &'ast Pattern,
    values: Values<'ast>,
    bind: &mut impl FnMut(&'ast str, Values<'ast>),
) {
    match pattern {
        Pattern::MatchAs(pattern) => {
            if let Some(inner) = &pattern.pattern {
                bind_pattern(inner, values.clone(), bind);
            }
            if let Some(name) = &pattern.name {
                bind(name.as_str(), values);
            }
        },
        Pattern::MatchOr(pattern) => {
            for alternative in &pattern.patterns {
                bind_pattern(alternative, values.clone(), bind);
            }
        },
        Pattern::MatchSequence(pattern) => {
            let star = pattern
                .patterns
                .iter()
                .position(|p| matches!(p, Pattern::MatchStar(_)));
            let mut parts: Vec<Values<'ast>> = vec![Vec::new(); pattern.patterns.len()];
            for value in &values {
                if let Value::Bag(elts) = value {
                    spread(star, elts, &mut parts);
                } else if let Value::Seq(elts) = value {
                    split_sequence(pattern.patterns.len(), star, elts, &mut parts);
                }
            }
            for (inner, part) in pattern.patterns.iter().zip(parts) {
                bind_pattern(inner, part, bind);
            }
        },
        Pattern::MatchStar(pattern) => {
            if let Some(name) = &pattern.name {
                bind(name.as_str(), values);
            }
        },
        // A class or mapping pattern's captures hold attributes or items the
        // analysis does not follow.
        Pattern::MatchClass(pattern) => {
            for inner in pattern.arguments.patterns.iter().chain(
                pattern
                    .arguments
                    .keywords
                    .iter()
                    .map(|keyword| &keyword.pattern),
            ) {
                bind_pattern(inner, Vec::new(), bind);
            }
        },
        Pattern::MatchMapping(pattern) => {
            for inner in &pattern.patterns {
                bind_pattern(inner, Vec::new(), bind);
            }
            if let Some(rest) = &pattern.rest {
                bind(rest.as_str(), Vec::new());
            }
        },
        Pattern::MatchValue(_) | Pattern::MatchSingleton(_) => {},
    }
}

/// A module-level class: its attributes, methods and what its methods
/// assign to `self`.
#[derive(Default)]
struct Class<'ast> {
    attributes: FxHashMap<&'ast str, Vec<&'ast Expr>>,
    methods: FxHashMap<&'ast str, &'ast StmtFunctionDef>,
    self_attributes: FxHashMap<&'ast str, Vec<(&'ast StmtFunctionDef, &'ast Expr)>>,
}

impl<'ast> Class<'ast> {
    fn build(body: &'ast [Stmt]) -> Self {
        let mut class = Self::default();
        for stmt in flatten(body) {
            let (targets, value): (Vec<&'ast Expr>, &'ast Expr) = match stmt {
                Stmt::FunctionDef(def) => {
                    // The last definition wins, as when the body runs; `@overload`
                    // stubs come before the implementation.
                    class.methods.insert(def.name.as_str(), def);
                    let assignments = self_parameter(def)
                        .map(|this| self_assignments(&def.body, this))
                        .unwrap_or_default();
                    for (attribute, value) in assignments {
                        class
                            .self_attributes
                            .entry(attribute)
                            .or_default()
                            .push((def, value));
                    }
                    continue;
                },
                Stmt::Assign(assign) => (assign.targets.iter().collect(), &assign.value),
                Stmt::AnnAssign(assign) => match &assign.value {
                    Some(value) => (vec![&*assign.target], value),
                    None => continue,
                },
                _ => continue,
            };
            for target in targets {
                if let Expr::Name(name) = target {
                    class
                        .attributes
                        .entry(name.id.as_str())
                        .or_default()
                        .push(value);
                }
            }
        }
        class
    }
}

/// The module-level functions and classes, whose bodies a call is
/// evaluated through.
#[derive(Default)]
pub(super) struct Program<'ast> {
    /// Every definition of each name: one in each branch of a top-level
    /// `if` or `try` may be the one bound.
    functions: FxHashMap<&'ast str, Vec<&'ast StmtFunctionDef>>,
    classes: FxHashMap<&'ast str, Class<'ast>>,
}

impl<'ast> Program<'ast> {
    pub(super) fn build(stmts: &'ast [Stmt]) -> Self {
        let mut program = Self::default();
        for stmt in flatten(stmts) {
            match stmt {
                Stmt::FunctionDef(def) => {
                    program
                        .functions
                        .entry(def.name.as_str())
                        .or_default()
                        .push(def);
                },
                Stmt::ClassDef(class) => {
                    program
                        .classes
                        .insert(class.name.as_str(), Class::build(&class.body));
                },
                _ => {},
            }
        }
        program
    }

    /// Whether `name` is a module-level function or class.
    pub(super) fn defines(&self, name: &str) -> bool {
        self.functions.contains_key(name) || self.classes.contains_key(name)
    }

    pub(super) fn functions(
        &self,
    ) -> impl Iterator<Item = (&'ast str, &'ast StmtFunctionDef)> + '_ {
        self.functions
            .iter()
            .flat_map(|(&name, defs)| defs.iter().map(move |&def| (name, def)))
    }
}

/// The statements of `body` and of the blocks nested in it, without the
/// bodies of nested functions and classes.
fn flatten(body: &[Stmt]) -> Vec<&Stmt> {
    let mut out = Vec::new();
    let mut stack: Vec<&Stmt> = body.iter().rev().collect();
    while let Some(stmt) = stack.pop() {
        out.push(stmt);
        let nested: Vec<&[Stmt]> = match stmt {
            Stmt::If(stmt) => std::iter::once(&stmt.body[..])
                .chain(stmt.elif_else_clauses.iter().map(|clause| &clause.body[..]))
                .collect(),
            Stmt::For(stmt) => vec![&stmt.body, &stmt.orelse],
            Stmt::While(stmt) => vec![&stmt.body, &stmt.orelse],
            Stmt::With(stmt) => vec![&stmt.body],
            Stmt::Try(stmt) => std::iter::once(&stmt.body[..])
                .chain(stmt.handlers.iter().map(|handler| {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                    &handler.body[..]
                }))
                .chain([&stmt.orelse[..], &stmt.finalbody[..]])
                .collect(),
            Stmt::Match(stmt) => stmt.cases.iter().map(|case| &case.body[..]).collect(),
            _ => Vec::new(),
        };
        for block in nested.into_iter().rev() {
            stack.extend(block.iter().rev());
        }
    }
    out
}

/// The name a method's first parameter binds the instance to, unless it is
/// a static or class method.
fn self_parameter(def: &StmtFunctionDef) -> Option<&str> {
    if def.decorator_list.iter().any(|decorator| {
        decorator_is(&decorator.expression, "staticmethod")
            || decorator_is(&decorator.expression, "classmethod")
    }) {
        return None;
    }
    def.parameters
        .posonlyargs
        .iter()
        .chain(&def.parameters.args)
        .next()
        .map(|parameter| parameter.parameter.name.as_str())
}

fn decorator_is(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Name(decorator) => decorator.id.as_str() == name,
        Expr::Attribute(decorator) => decorator.attr.as_str() == name,
        _ => false,
    }
}

/// `self.attr = value` in a method body.
fn self_assignments<'ast>(body: &'ast [Stmt], this: &str) -> Vec<(&'ast str, &'ast Expr)> {
    let mut out = Vec::new();
    for stmt in flatten(body) {
        let (targets, value): (Vec<&Expr>, &Expr) = match stmt {
            Stmt::Assign(assign) => (assign.targets.iter().collect(), &assign.value),
            Stmt::AnnAssign(assign) => match &assign.value {
                Some(value) => (vec![&assign.target], value),
                None => continue,
            },
            _ => continue,
        };
        for target in targets {
            if let Expr::Attribute(attribute) = target
                && matches!(&*attribute.value, Expr::Name(name) if name.id.as_str() == this)
            {
                out.push((attribute.attr.as_str(), value));
            }
        }
    }
    out
}

/// Where the evaluation stands on a name outside any function it follows.
pub(super) enum Lookup<'ast> {
    /// A name the walk tracked, with what it holds.
    Bound(Values<'ast>),
    /// A name the current scope binds to something not tracked.
    Local,
    /// Neither: read as the dotted name itself.
    Free,
}

type Frame<'ast> = FxHashMap<&'ast str, Values<'ast>>;

/// What a call passes: the bound instance or class, then its arguments.
#[derive(Default)]
struct Args<'ast> {
    this: Option<Value<'ast>>,
    positional: Vec<Values<'ast>>,
    keywords: Vec<(&'ast str, Values<'ast>)>,
}

impl<'ast> Args<'ast> {
    fn this(this: Value<'ast>) -> Self {
        Self {
            this: Some(this),
            ..Self::default()
        }
    }

    fn keyword(&self, name: &str) -> Option<Values<'ast>> {
        self.keywords
            .iter()
            .find(|(keyword, _)| *keyword == name)
            .map(|(_, values)| values.clone())
    }
}

/// Evaluates expressions in the module the walk is in.
pub(super) struct Evaluator<'e, 'ast> {
    pub(super) program: &'e Program<'ast>,
    /// A name read where the walk is.
    pub(super) here: &'e dyn Fn(&str) -> Lookup<'ast>,
    /// A name read at module level, from a function body.
    pub(super) module: &'e dyn Fn(&str) -> Option<Values<'ast>>,
    /// What was assigned to each dotted attribute (`pkg.reg.h = pkg.m`).
    pub(super) stores: &'e FxHashMap<String, Values<'ast>>,
    /// How many more function bodies this evaluation may follow.
    pub(super) calls: &'e Cell<u32>,
    /// How many more expressions may nest inside the one evaluated.
    pub(super) nesting: &'e Cell<u32>,
}

impl<'ast> Evaluator<'_, 'ast> {
    pub(super) fn eval(&self, expr: &'ast Expr) -> Values<'ast> {
        self.eval_in(expr, None, 0)
    }

    /// What `def` assigns to the names it declares `global`.
    pub(super) fn global_stores(
        &self,
        def: &'ast StmtFunctionDef,
    ) -> Vec<(&'ast str, Values<'ast>)> {
        let declared: Vec<&'ast str> = flatten(&def.body)
            .into_iter()
            .filter_map(|stmt| match stmt {
                Stmt::Global(global) => {
                    Some(global.names.iter().map(ruff_python_ast::Identifier::as_str))
                },
                _ => None,
            })
            .flatten()
            .collect();
        if declared.is_empty() {
            return Vec::new();
        }
        let mut frame = self.frame(def, &Args::default(), 0);
        declared
            .into_iter()
            .filter_map(|name| Some((name, frame.remove(name)?)))
            .collect()
    }

    /// What `def` returns when called with arguments it knows nothing of.
    pub(super) fn returns(&self, def: &'ast StmtFunctionDef) -> Values<'ast> {
        self.invoke(def, &Args::default(), 0)
    }

    fn eval_in(&self, expr: &'ast Expr, frame: Option<&Frame<'ast>>, depth: u32) -> Values<'ast> {
        let Some(left) = self.nesting.get().checked_sub(1) else {
            return Vec::new();
        };
        self.nesting.set(left);
        let out = self.eval_nested(expr, frame, depth);
        self.nesting.set(left + 1);
        out
    }

    fn eval_nested(
        &self,
        expr: &'ast Expr,
        frame: Option<&Frame<'ast>>,
        depth: u32,
    ) -> Values<'ast> {
        let mut out = Vec::new();
        match expr {
            Expr::Name(name) => out = self.name(name.id.as_str(), frame),
            Expr::Attribute(attribute) => {
                for value in self.eval_in(&attribute.value, frame, depth) {
                    self.attribute(value, attribute.attr.as_str(), depth, &mut out);
                }
            },
            Expr::Call(call) => {
                for value in self.eval_in(&call.func, frame, depth) {
                    extend(&mut out, self.call(value, call, frame, depth));
                }
            },
            Expr::If(if_expr) => {
                out = self.eval_in(&if_expr.body, frame, depth);
                extend(&mut out, self.eval_in(&if_expr.orelse, frame, depth));
            },
            Expr::BoolOp(bool_op) => {
                for value in &bool_op.values {
                    extend(&mut out, self.eval_in(value, frame, depth));
                }
            },
            Expr::Named(named) => out = self.eval_in(&named.value, frame, depth),
            Expr::List(list) => out.push(self.display(&list.elts, frame, depth)),
            Expr::Tuple(tuple) => out.push(self.display(&tuple.elts, frame, depth)),
            Expr::Subscript(subscript) => {
                let index = int_index(&subscript.slice);
                for value in self.eval_in(&subscript.value, frame, depth) {
                    let elts = match value {
                        Value::Seq(elts) => elts,
                        Value::Bag(elts) => {
                            extend(&mut out, elts);
                            continue;
                        },
                        _ => continue,
                    };
                    let picked = index.and_then(|index| {
                        let len = i64::try_from(elts.len()).ok()?;
                        let at = if index < 0 { len + index } else { index };
                        usize::try_from(at).ok().and_then(|at| elts.get(at))
                    });
                    match picked {
                        Some(elt) => extend(&mut out, elt.clone()),
                        None => extend(&mut out, elements(&[Value::Seq(elts)])),
                    }
                }
            },
            Expr::ListComp(comp) => {
                out.push(Value::Bag(self.comprehension(
                    &comp.elt,
                    &comp.generators,
                    frame,
                    depth,
                )));
            },
            Expr::SetComp(comp) => {
                out.push(Value::Bag(self.comprehension(
                    &comp.elt,
                    &comp.generators,
                    frame,
                    depth,
                )));
            },
            Expr::Generator(comp) => {
                out.push(Value::Bag(self.comprehension(
                    &comp.elt,
                    &comp.generators,
                    frame,
                    depth,
                )));
            },
            Expr::Lambda(lambda) => out.push(Value::Lambda(lambda)),
            _ => {},
        }
        out
    }

    fn display(&self, elts: &'ast [Expr], frame: Option<&Frame<'ast>>, depth: u32) -> Value<'ast> {
        let mut values = Vec::with_capacity(elts.len());
        let mut starred = false;
        for elt in elts {
            match elt {
                Expr::Starred(star) => {
                    starred = true;
                    values.push(elements(&self.eval_in(&star.value, frame, depth)));
                },
                _ => values.push(self.eval_in(elt, frame, depth)),
            }
        }
        if starred {
            // A starred element moves the ones after it by its length.
            let mut any = Vec::new();
            for elt in values {
                extend(&mut any, elt);
            }
            return Value::Bag(any);
        }
        Value::Seq(values)
    }

    fn comprehension(
        &self,
        elt: &'ast Expr,
        generators: &'ast [Comprehension],
        frame: Option<&Frame<'ast>>,
        depth: u32,
    ) -> Values<'ast> {
        let mut inner: Frame<'ast> = frame.cloned().unwrap_or_default();
        for generator in generators {
            let items = elements(&self.eval_comprehension_part(
                &generator.iter,
                &inner,
                frame.is_some(),
                depth,
            ));
            destructure(&generator.target, items, &mut |name, values| {
                inner.insert(name, values);
            });
        }
        self.eval_comprehension_part(elt, &inner, frame.is_some(), depth)
    }

    /// `expr` in a comprehension that binds `inner`. Outside a function the
    /// names it does not bind are read where the walk is.
    fn eval_comprehension_part(
        &self,
        expr: &'ast Expr,
        inner: &Frame<'ast>,
        in_function: bool,
        depth: u32,
    ) -> Values<'ast> {
        if in_function {
            return self.eval_in(expr, Some(inner), depth);
        }
        let here = |name: &str| match inner.get(name) {
            Some(values) => Lookup::Bound(values.clone()),
            None => (self.here)(name),
        };
        Evaluator {
            program: self.program,
            here: &here,
            module: self.module,
            stores: self.stores,
            calls: self.calls,
            nesting: self.nesting,
        }
        .eval_in(expr, None, depth)
    }

    fn name(&self, name: &str, frame: Option<&Frame<'ast>>) -> Values<'ast> {
        if let Some(frame) = frame {
            if let Some(values) = frame.get(name) {
                return values.clone();
            }
            return (self.module)(name).unwrap_or_else(|| vec![Value::Dotted(name.to_owned())]);
        }
        match (self.here)(name) {
            Lookup::Bound(values) => values,
            Lookup::Local => Vec::new(),
            Lookup::Free => vec![Value::Dotted(name.to_owned())],
        }
    }

    fn attribute(&self, value: Value<'ast>, attr: &str, depth: u32, out: &mut Values<'ast>) {
        match value {
            Value::Dotted(text) => {
                if let Some((&class, _)) = self.program.classes.get_key_value(text.as_str()) {
                    extend(out, self.member(class, attr, false, depth));
                }
                let dotted = format!("{text}.{attr}");
                if let Some(stored) = self.stores.get(&dotted) {
                    extend(out, stored.clone());
                }
                push(out, Value::Dotted(dotted));
            },
            Value::Instance(class) => extend(out, self.member(class, attr, true, depth)),
            Value::Method(..) | Value::Lambda(_) | Value::Seq(_) | Value::Bag(_) => {},
        }
    }

    /// `attr` of a module-level class or, with `instance`, of its instance.
    fn member(&self, class: &'ast str, attr: &str, instance: bool, depth: u32) -> Values<'ast> {
        let mut out = Vec::new();
        let out = &mut out;
        let Some(info) = self.program.classes.get(class) else {
            return Vec::new();
        };
        // A member's value may read the member itself (`self.a = self.a or
        // {}`), so each lookup spends the call budget.
        if !self.spend(depth) {
            return Vec::new();
        }
        for value in info.attributes.get(attr).into_iter().flatten() {
            extend(out, self.eval_in(value, Some(&Frame::default()), depth + 1));
        }
        if let Some((&name, def)) = info.methods.get_key_value(attr) {
            let property = def.decorator_list.iter().any(|decorator| {
                decorator_is(&decorator.expression, "property")
                    || decorator_is(&decorator.expression, "cached_property")
            });
            if property && instance {
                extend(
                    out,
                    self.invoke(def, &Args::this(Value::Instance(class)), depth),
                );
            } else {
                push(out, Value::Method(class, name, instance));
            }
        }
        if instance {
            for (method, value) in info.self_attributes.get(attr).into_iter().flatten() {
                let frame = self.frame(method, &Args::this(Value::Instance(class)), depth + 1);
                extend(out, self.eval_in(value, Some(&frame), depth + 2));
            }
        }
        std::mem::take(out)
    }

    fn call(
        &self,
        callee: Value<'ast>,
        call: &'ast ExprCall,
        frame: Option<&Frame<'ast>>,
        depth: u32,
    ) -> Values<'ast> {
        let arguments = |this: Option<Value<'ast>>| -> Args<'ast> {
            let mut positional = Vec::new();
            for arg in &call.arguments.args {
                if arg.is_starred_expr() {
                    break;
                }
                positional.push(self.eval_in(arg, frame, depth));
            }
            let keywords = call
                .arguments
                .keywords
                .iter()
                .filter_map(|keyword| {
                    Some((
                        keyword.arg.as_ref()?.as_str(),
                        self.eval_in(&keyword.value, frame, depth),
                    ))
                })
                .collect();
            Args {
                this,
                positional,
                keywords,
            }
        };
        match callee {
            Value::Dotted(text) => {
                if let Some(defs) = self.program.functions.get(text.as_str()) {
                    let args = arguments(None);
                    let mut out = Vec::new();
                    for def in defs {
                        extend(&mut out, self.invoke(def, &args, depth));
                    }
                    out
                } else if let Some((&class, _)) = self.program.classes.get_key_value(text.as_str())
                {
                    vec![Value::Instance(class)]
                } else {
                    vec![Value::Dotted(format!("{text}.()"))]
                }
            },
            Value::Method(class, name, instance) => {
                let Some(def) = self
                    .program
                    .classes
                    .get(class)
                    .and_then(|info| info.methods.get(name))
                else {
                    return Vec::new();
                };
                let decorated = |name| {
                    def.decorator_list
                        .iter()
                        .any(|decorator| decorator_is(&decorator.expression, name))
                };
                let this = if decorated("staticmethod") {
                    None
                } else if decorated("classmethod") {
                    Some(Value::Dotted(class.to_owned()))
                } else if instance {
                    Some(Value::Instance(class))
                } else {
                    None
                };
                self.invoke(def, &arguments(this), depth)
            },
            Value::Lambda(lambda) => {
                if depth >= MAX_CALL_DEPTH {
                    return Vec::new();
                }
                let mut inner = Frame::default();
                if let Some(parameters) = &lambda.parameters {
                    self.bind_parameters(parameters, &arguments(None), &mut inner, depth);
                }
                self.eval_in(&lambda.body, Some(&inner), depth + 1)
            },
            Value::Instance(_) | Value::Seq(_) | Value::Bag(_) => Vec::new(),
        }
    }

    /// What `def` returns when called with these arguments.
    fn invoke(&self, def: &'ast StmtFunctionDef, args: &Args<'ast>, depth: u32) -> Values<'ast> {
        if !self.spend(depth) {
            return Vec::new();
        }
        let frame = self.frame(def, args, depth);
        let mut out = Vec::new();
        for stmt in flatten(&def.body) {
            if let Stmt::Return(ret) = stmt
                && let Some(value) = &ret.value
            {
                extend(&mut out, self.eval_in(value, Some(&frame), depth + 1));
            }
        }
        out
    }

    /// Takes one body from the call budget, unless it or the depth ran out.
    fn spend(&self, depth: u32) -> bool {
        if depth >= MAX_CALL_DEPTH || self.calls.get() == 0 {
            return false;
        }
        self.calls.set(self.calls.get() - 1);
        true
    }

    /// The names `def`'s body sees: its parameters bound to the arguments,
    /// then what its assignments and loops bind, in order.
    fn frame(&self, def: &'ast StmtFunctionDef, args: &Args<'ast>, depth: u32) -> Frame<'ast> {
        let mut frame = Frame::default();
        self.bind_parameters(&def.parameters, args, &mut frame, depth);
        for stmt in flatten(&def.body) {
            let mut bound: Vec<(&'ast str, Values<'ast>)> = Vec::new();
            let mut collect = |name, values| bound.push((name, values));
            match stmt {
                Stmt::Assign(assign) => {
                    let values = self.eval_in(&assign.value, Some(&frame), depth + 1);
                    for target in &assign.targets {
                        destructure(target, values.clone(), &mut collect);
                    }
                },
                Stmt::AnnAssign(assign) => {
                    let values = assign
                        .value
                        .as_ref()
                        .map(|value| self.eval_in(value, Some(&frame), depth + 1))
                        .unwrap_or_default();
                    destructure(&assign.target, values, &mut collect);
                },
                Stmt::For(for_stmt) => {
                    let items = elements(&self.eval_in(&for_stmt.iter, Some(&frame), depth + 1));
                    destructure(&for_stmt.target, items, &mut collect);
                },
                Stmt::Match(match_stmt) => {
                    let subject = self.eval_in(&match_stmt.subject, Some(&frame), depth + 1);
                    for case in &match_stmt.cases {
                        bind_pattern(&case.pattern, subject.clone(), &mut collect);
                    }
                },
                Stmt::With(with_stmt) => {
                    for item in &with_stmt.items {
                        if let Some(vars) = &item.optional_vars {
                            destructure(vars, Vec::new(), &mut collect);
                        }
                    }
                },
                Stmt::FunctionDef(inner) => collect(inner.name.as_str(), Vec::new()),
                Stmt::ClassDef(inner) => collect(inner.name.as_str(), Vec::new()),
                _ => {},
            }
            for (name, values) in bound {
                extend(frame.entry(name).or_default(), values);
            }
        }
        frame
    }

    fn bind_parameters(
        &self,
        parameters: &'ast Parameters,
        args: &Args<'ast>,
        frame: &mut Frame<'ast>,
        depth: u32,
    ) {
        let mut arguments = args
            .this
            .clone()
            .map(|this| vec![this])
            .into_iter()
            .chain(args.positional.iter().cloned());
        for parameter in parameters.posonlyargs.iter().chain(&parameters.args) {
            let name = parameter.parameter.name.as_str();
            let values = arguments
                .next()
                .or_else(|| args.keyword(name))
                .or_else(|| {
                    parameter
                        .default
                        .as_deref()
                        .map(|default| self.eval_in(default, Some(&Frame::default()), depth + 1))
                })
                .unwrap_or_default();
            frame.insert(name, values);
        }
        let rest: Vec<Values<'ast>> = arguments.collect();
        if let Some(vararg) = &parameters.vararg {
            frame.insert(vararg.name.as_str(), vec![Value::Seq(rest)]);
        }
        for parameter in &parameters.kwonlyargs {
            let name = parameter.parameter.name.as_str();
            let values =
                args.keyword(name)
                    .or_else(|| {
                        parameter.default.as_deref().map(|default| {
                            self.eval_in(default, Some(&Frame::default()), depth + 1)
                        })
                    })
                    .unwrap_or_default();
            frame.insert(name, values);
        }
        if let Some(kwarg) = &parameters.kwarg {
            frame.insert(kwarg.name.as_str(), Vec::new());
        }
    }
}

/// A literal integer subscript, negative ones included.
fn int_index(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::NumberLiteral(ExprNumberLiteral {
            value: Number::Int(int),
            ..
        }) => int.as_i64(),
        Expr::UnaryOp(unary) if matches!(unary.op, UnaryOp::USub) => {
            int_index(&unary.operand).map(|index| -index)
        },
        _ => None,
    }
}

/// `expr` as a dotted name, when it is one.
pub(super) fn dotted_text(expr: &Expr) -> Option<String> {
    let mut text = String::new();
    write_receiver(expr, &mut text).then_some(text)
}

/// A fresh call and nesting budget for one top-level evaluation.
pub(super) fn budget() -> (Cell<u32>, Cell<u32>) {
    (Cell::new(MAX_CALLS), Cell::new(MAX_NESTING))
}
