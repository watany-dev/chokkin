//! Nesting limit for the syntax tree walks (#1045).
//!
//! `ruff_python_parser` grows its own stack, so it accepts a tree of any
//! depth, but every walk over the tree (the module visitor, the guard and
//! annotation walks, the `setup.py` evaluator) recurses once per level on a
//! fixed thread stack, and so does dropping the tree. A tree past that stack
//! aborts the whole process, so a tree deeper than [`MAX_AST_DEPTH`] is
//! neither walked nor dropped: the caller treats the source as unreadable,
//! like a syntax error, and the tree is leaked.

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_pattern, walk_stmt};
use ruff_python_ast::{Expr, Mod, ModModule, Pattern, PySourceType, Stmt};
use ruff_python_parser::{Mode, ParseError, ParseOptions, Parsed};
use ruff_text_size::{Ranged, TextSize};

/// Statement, expression and pattern levels a walk may nest. `CPython` rejects
/// 200 nested brackets and fails to compile a few thousand nested operators,
/// so real code stays far below it; chokkin's walks overflow a 1 MiB stack
/// (Windows' main thread) at about 2,000 levels.
pub(crate) const MAX_AST_DEPTH: usize = 1_000;

/// Why a source yields no tree to walk.
#[derive(Debug)]
pub(crate) enum Unwalkable {
    /// The first syntax error; the parser reads nothing past it.
    Syntax(ParseError),
    /// Start of the first node nested deeper than [`MAX_AST_DEPTH`].
    TooDeep(TextSize),
}

/// [`ruff_python_parser::parse_module`] for a tree chokkin walks.
pub(crate) fn parse_walkable_module(source: &str) -> Result<Parsed<ModModule>, Unwalkable> {
    let parsed = ruff_python_parser::parse_unchecked_source(source, PySourceType::Python);
    let too_deep = suite_too_deep(&parsed.syntax().body);
    checked(parsed, too_deep)
}

/// The expression `source` holds, unless it does not parse or nests deeper
/// than [`MAX_AST_DEPTH`].
pub(crate) fn parse_walkable_expression(source: &str) -> Option<Expr> {
    let parsed = ruff_python_parser::parse_unchecked(source, ParseOptions::from(Mode::Expression));
    let too_deep = expr_too_deep(&parsed.syntax().as_expression()?.body);
    let Mod::Expression(expression) = checked(parsed, too_deep).ok()?.into_syntax() else {
        return None;
    };
    Some(*expression.body)
}

fn checked<T>(parsed: Parsed<T>, too_deep: Option<TextSize>) -> Result<Parsed<T>, Unwalkable> {
    let error = parsed.errors().first().cloned();
    if let Some(start) = too_deep {
        #[expect(
            clippy::mem_forget,
            reason = "dropping recurses like a walk; a one-shot process can afford the leak"
        )]
        std::mem::forget(parsed);
        return Err(error.map_or(Unwalkable::TooDeep(start), Unwalkable::Syntax));
    }
    match error {
        Some(error) => Err(Unwalkable::Syntax(error)),
        None => Ok(parsed),
    }
}

/// Start of the first node nested deeper than [`MAX_AST_DEPTH`] in `stmts`.
fn suite_too_deep(stmts: &[Stmt]) -> Option<TextSize> {
    let mut walk = DepthWalk::default();
    walk.visit_body(stmts);
    walk.too_deep
}

/// Start of the first node nested deeper than [`MAX_AST_DEPTH`] in `expr`.
fn expr_too_deep(expr: &Expr) -> Option<TextSize> {
    let mut walk = DepthWalk::default();
    walk.visit_expr(expr);
    walk.too_deep
}

/// Stops descending at the limit, so the walk itself stays within it.
#[derive(Default)]
struct DepthWalk {
    depth: usize,
    too_deep: Option<TextSize>,
}

impl DepthWalk {
    fn enter(&mut self, node: &impl Ranged, walk: impl FnOnce(&mut Self)) {
        if self.too_deep.is_some() {
            return;
        }
        if self.depth == MAX_AST_DEPTH {
            self.too_deep = Some(node.start());
            return;
        }
        self.depth += 1;
        walk(self);
        self.depth -= 1;
    }
}

impl<'a> Visitor<'a> for DepthWalk {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        self.enter(stmt, |walk| walk_stmt(walk, stmt));
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        self.enter(expr, |walk| walk_expr(walk, expr));
    }

    fn visit_pattern(&mut self, pattern: &'a Pattern) {
        self.enter(pattern, |walk| walk_pattern(walk, pattern));
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_AST_DEPTH, Unwalkable, parse_walkable_expression, parse_walkable_module};

    fn suite(source: &str) -> Option<usize> {
        match parse_walkable_module(source) {
            Ok(_) => None,
            Err(Unwalkable::TooDeep(start)) => Some(start.into()),
            Err(Unwalkable::Syntax(error)) => panic!("{error}"),
        }
    }

    #[test]
    fn nesting_at_the_limit_is_walked() {
        // The statement is one level, the list literals the rest.
        let lists = MAX_AST_DEPTH - 1;
        let source = format!("x = {}{}\n", "[".repeat(lists), "]".repeat(lists));
        assert_eq!(suite(&source), None);
    }

    #[test]
    fn nesting_past_the_limit_names_the_first_deep_node() {
        let source = format!("x = {}1\n", "-".repeat(MAX_AST_DEPTH));
        // `x = ` is 4 bytes; the statement and 999 unary operators fit.
        assert_eq!(suite(&source), Some(4 + MAX_AST_DEPTH - 1));
    }

    #[test]
    fn statements_and_patterns_count_as_levels() {
        let mut ifs = String::new();
        for level in 0..MAX_AST_DEPTH {
            ifs.push_str(&" ".repeat(level));
            ifs.push_str("if x:\n");
        }
        assert!(suite(&format!("{ifs}{}pass\n", " ".repeat(MAX_AST_DEPTH))).is_some());

        let pattern = format!(
            "{}x{}",
            "[".repeat(MAX_AST_DEPTH),
            "]".repeat(MAX_AST_DEPTH)
        );
        assert!(suite(&format!("match y:\n    case {pattern}:\n        pass\n")).is_some());
    }

    #[test]
    fn expressions_are_measured_on_their_own() {
        assert!(parse_walkable_expression(&format!("{}1", "+".repeat(MAX_AST_DEPTH))).is_none());
        assert!(parse_walkable_expression("a[b]").is_some());
        assert!(parse_walkable_expression("a[").is_none());
    }

    #[test]
    fn a_deep_tree_with_a_syntax_error_reports_the_error_without_dropping_it() {
        let source = format!("x = {}1 +\n", "-".repeat(100_000));
        assert!(matches!(
            parse_walkable_module(&source),
            Err(Unwalkable::Syntax(_))
        ));
    }
}
