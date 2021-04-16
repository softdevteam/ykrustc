use clippy_utils::diagnostics::span_lint_and_sugg;
use clippy_utils::ty::implements_trait;
use clippy_utils::{get_trait_def_id, match_qpath, paths, sugg};
use if_chain::if_chain;
use rustc_errors::Applicability;
use rustc_hir as hir;
use rustc_hir::ExprKind;
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty::Ty;
use rustc_span::sym;

use super::FROM_ITER_INSTEAD_OF_COLLECT;

pub(super) fn check(cx: &LateContext<'_>, expr: &hir::Expr<'_>, args: &[hir::Expr<'_>], func_kind: &ExprKind<'_>) {
    if_chain! {
        if let hir::ExprKind::Path(path) = func_kind;
        if match_qpath(path, &["from_iter"]);
        let ty = cx.typeck_results().expr_ty(expr);
        let arg_ty = cx.typeck_results().expr_ty(&args[0]);
        if let Some(from_iter_id) = get_trait_def_id(cx, &paths::FROM_ITERATOR);
        if let Some(iter_id) = cx.tcx.get_diagnostic_item(sym::Iterator);

        if implements_trait(cx, ty, from_iter_id, &[]) && implements_trait(cx, arg_ty, iter_id, &[]);
        then {
            // `expr` implements `FromIterator` trait
            let iter_expr = sugg::Sugg::hir(cx, &args[0], "..").maybe_par();
            let turbofish = extract_turbofish(cx, expr, ty);
            let sugg = format!("{}.collect::<{}>()", iter_expr, turbofish);
            span_lint_and_sugg(
                cx,
                FROM_ITER_INSTEAD_OF_COLLECT,
                expr.span,
                "usage of `FromIterator::from_iter`",
                "use `.collect()` instead of `::from_iter()`",
                sugg,
                Applicability::MaybeIncorrect,
            );
        }
    }
}

fn extract_turbofish(cx: &LateContext<'_>, expr: &hir::Expr<'_>, ty: Ty<'tcx>) -> String {
    let call_site = expr.span.source_callsite();
    if_chain! {
        if let Ok(snippet) = cx.sess().source_map().span_to_snippet(call_site);
        let snippet_split = snippet.split("::").collect::<Vec<_>>();
        if let Some((_, elements)) = snippet_split.split_last();

        then {
            // is there a type specifier? (i.e.: like `<u32>` in `collections::BTreeSet::<u32>::`)
            if let Some(type_specifier) = snippet_split.iter().find(|e| e.starts_with('<') && e.ends_with('>')) {
                // remove the type specifier from the path elements
                let without_ts = elements.iter().filter_map(|e| {
                    if e == type_specifier { None } else { Some((*e).to_string()) }
                }).collect::<Vec<_>>();
                // join and add the type specifier at the end (i.e.: `collections::BTreeSet<u32>`)
                format!("{}{}", without_ts.join("::"), type_specifier)
            } else {
                // type is not explicitly specified so wildcards are needed
                // i.e.: 2 wildcards in `std::collections::BTreeMap<&i32, &char>`
                let ty_str = ty.to_string();
                let start = ty_str.find('<').unwrap_or(0);
                let end = ty_str.find('>').unwrap_or_else(|| ty_str.len());
                let nb_wildcard = ty_str[start..end].split(',').count();
                let wildcards = format!("_{}", ", _".repeat(nb_wildcard - 1));
                format!("{}<{}>", elements.join("::"), wildcards)
            }
        } else {
            ty.to_string()
        }
    }
}
