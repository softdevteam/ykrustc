use super::utils::make_iterator_snippet;
use super::MANUAL_FLATTEN;
use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::{is_ok_ctor, is_some_ctor, path_to_local_id};
use if_chain::if_chain;
use rustc_errors::Applicability;
use rustc_hir::{Expr, ExprKind, MatchSource, Pat, PatKind, QPath, StmtKind};
use rustc_lint::LateContext;
use rustc_middle::ty;
use rustc_span::source_map::Span;

/// Check for unnecessary `if let` usage in a for loop where only the `Some` or `Ok` variant of the
/// iterator element is used.
pub(super) fn check<'tcx>(
    cx: &LateContext<'tcx>,
    pat: &'tcx Pat<'_>,
    arg: &'tcx Expr<'_>,
    body: &'tcx Expr<'_>,
    span: Span,
) {
    if let ExprKind::Block(block, _) = body.kind {
        // Ensure the `if let` statement is the only expression or statement in the for-loop
        let inner_expr = if block.stmts.len() == 1 && block.expr.is_none() {
            let match_stmt = &block.stmts[0];
            if let StmtKind::Semi(inner_expr) = match_stmt.kind {
                Some(inner_expr)
            } else {
                None
            }
        } else if block.stmts.is_empty() {
            block.expr
        } else {
            None
        };

        if_chain! {
            if let Some(inner_expr) = inner_expr;
            if let ExprKind::Match(
                match_expr, match_arms, MatchSource::IfLetDesugar{ contains_else_clause: false }
            ) = inner_expr.kind;
            // Ensure match_expr in `if let` statement is the same as the pat from the for-loop
            if let PatKind::Binding(_, pat_hir_id, _, _) = pat.kind;
            if path_to_local_id(match_expr, pat_hir_id);
            // Ensure the `if let` statement is for the `Some` variant of `Option` or the `Ok` variant of `Result`
            if let PatKind::TupleStruct(QPath::Resolved(None, path), _, _) = match_arms[0].pat.kind;
            let some_ctor = is_some_ctor(cx, path.res);
            let ok_ctor = is_ok_ctor(cx, path.res);
            if some_ctor || ok_ctor;
            then {
                let if_let_type = if some_ctor { "Some" } else { "Ok" };
                // Prepare the error message
                let msg = format!("unnecessary `if let` since only the `{}` variant of the iterator element is used", if_let_type);

                // Prepare the help message
                let mut applicability = Applicability::MaybeIncorrect;
                let arg_snippet = make_iterator_snippet(cx, arg, &mut applicability);
                let copied = match cx.typeck_results().expr_ty(match_expr).kind() {
                    ty::Ref(_, inner, _) => match inner.kind() {
                        ty::Ref(..) => ".copied()",
                        _ => ""
                    }
                    _ => ""
                };

                span_lint_and_then(
                    cx,
                    MANUAL_FLATTEN,
                    span,
                    &msg,
                    |diag| {
                        let sugg = format!("{}{}.flatten()", arg_snippet, copied);
                        diag.span_suggestion(
                            arg.span,
                            "try",
                            sugg,
                            Applicability::MaybeIncorrect,
                        );
                        diag.span_help(
                            inner_expr.span,
                            "...and remove the `if let` statement in the for loop",
                        );
                    }
                );
            }
        }
    }
}
