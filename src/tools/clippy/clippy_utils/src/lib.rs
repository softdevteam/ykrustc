#![feature(box_patterns)]
#![feature(in_band_lifetimes)]
#![feature(iter_zip)]
#![cfg_attr(bootstrap, feature(or_patterns))]
#![feature(rustc_private)]
#![recursion_limit = "512"]
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc, clippy::must_use_candidate)]

// FIXME: switch to something more ergonomic here, once available.
// (Currently there is no way to opt into sysroot crates without `extern crate`.)
extern crate rustc_ast;
extern crate rustc_ast_pretty;
extern crate rustc_data_structures;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_infer;
extern crate rustc_lexer;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_mir;
extern crate rustc_session;
extern crate rustc_span;
extern crate rustc_target;
extern crate rustc_trait_selection;
extern crate rustc_typeck;

#[macro_use]
pub mod sym_helper;

#[allow(clippy::module_name_repetitions)]
pub mod ast_utils;
pub mod attrs;
pub mod camel_case;
pub mod comparisons;
pub mod consts;
pub mod diagnostics;
pub mod eager_or_lazy;
pub mod higher;
mod hir_utils;
pub mod numeric_literal;
pub mod paths;
pub mod ptr;
pub mod qualify_min_const_fn;
pub mod source;
pub mod sugg;
pub mod ty;
pub mod usage;
pub mod visitors;

pub use self::attrs::*;
pub use self::hir_utils::{both, count_eq, eq_expr_value, over, SpanlessEq, SpanlessHash};

use std::collections::hash_map::Entry;
use std::hash::BuildHasherDefault;

use if_chain::if_chain;
use rustc_ast::ast::{self, Attribute, BorrowKind, LitKind};
use rustc_data_structures::fx::FxHashMap;
use rustc_hir as hir;
use rustc_hir::def::{CtorKind, CtorOf, DefKind, Res};
use rustc_hir::def_id::{DefId, LOCAL_CRATE};
use rustc_hir::intravisit::{self, NestedVisitorMap, Visitor};
use rustc_hir::{
    def, Arm, BindingAnnotation, Block, Body, Constness, Expr, ExprKind, FnDecl, GenericArgs, HirId, Impl, ImplItem,
    ImplItemKind, Item, ItemKind, LangItem, MatchSource, Node, Param, Pat, PatKind, Path, PathSegment, QPath,
    TraitItem, TraitItemKind, TraitRef, TyKind,
};
use rustc_lint::{LateContext, Level, Lint, LintContext};
use rustc_middle::hir::exports::Export;
use rustc_middle::hir::map::Map;
use rustc_middle::ty as rustc_ty;
use rustc_middle::ty::{layout::IntegerExt, DefIdTree, Ty, TyCtxt, TypeFoldable};
use rustc_semver::RustcVersion;
use rustc_session::Session;
use rustc_span::hygiene::{ExpnKind, MacroKind};
use rustc_span::source_map::original_sp;
use rustc_span::sym;
use rustc_span::symbol::{kw, Symbol};
use rustc_span::{Span, DUMMY_SP};
use rustc_target::abi::Integer;

use crate::consts::{constant, Constant};
use crate::ty::is_recursively_primitive_type;

pub fn parse_msrv(msrv: &str, sess: Option<&Session>, span: Option<Span>) -> Option<RustcVersion> {
    if let Ok(version) = RustcVersion::parse(msrv) {
        return Some(version);
    } else if let Some(sess) = sess {
        if let Some(span) = span {
            sess.span_err(span, &format!("`{}` is not a valid Rust version", msrv));
        }
    }
    None
}

pub fn meets_msrv(msrv: Option<&RustcVersion>, lint_msrv: &RustcVersion) -> bool {
    msrv.map_or(true, |msrv| msrv.meets(*lint_msrv))
}

#[macro_export]
macro_rules! extract_msrv_attr {
    (LateContext) => {
        extract_msrv_attr!(@LateContext, ());
    };
    (EarlyContext) => {
        extract_msrv_attr!(@EarlyContext);
    };
    (@$context:ident$(, $call:tt)?) => {
        fn enter_lint_attrs(&mut self, cx: &rustc_lint::$context<'tcx>, attrs: &'tcx [rustc_ast::ast::Attribute]) {
            use $crate::get_unique_inner_attr;
            match get_unique_inner_attr(cx.sess$($call)?, attrs, "msrv") {
                Some(msrv_attr) => {
                    if let Some(msrv) = msrv_attr.value_str() {
                        self.msrv = $crate::parse_msrv(
                            &msrv.to_string(),
                            Some(cx.sess$($call)?),
                            Some(msrv_attr.span),
                        );
                    } else {
                        cx.sess$($call)?.span_err(msrv_attr.span, "bad clippy attribute");
                    }
                },
                _ => (),
            }
        }
    };
}

/// Returns `true` if the two spans come from differing expansions (i.e., one is
/// from a macro and one isn't).
#[must_use]
pub fn differing_macro_contexts(lhs: Span, rhs: Span) -> bool {
    rhs.ctxt() != lhs.ctxt()
}

/// If the given expression is a local binding, find the initializer expression.
/// If that initializer expression is another local binding, find its initializer again.
/// This process repeats as long as possible (but usually no more than once). Initializer
/// expressions with adjustments are ignored. If this is not desired, use [`find_binding_init`]
/// instead.
///
/// Examples:
/// ```ignore
/// let abc = 1;
/// //        ^ output
/// let def = abc;
/// dbg!(def)
/// //   ^^^ input
///
/// // or...
/// let abc = 1;
/// let def = abc + 2;
/// //        ^^^^^^^ output
/// dbg!(def)
/// //   ^^^ input
/// ```
pub fn expr_or_init<'a, 'b, 'tcx: 'b>(cx: &LateContext<'tcx>, mut expr: &'a Expr<'b>) -> &'a Expr<'b> {
    while let Some(init) = path_to_local(expr)
        .and_then(|id| find_binding_init(cx, id))
        .filter(|init| cx.typeck_results().expr_adjustments(init).is_empty())
    {
        expr = init;
    }
    expr
}

/// Finds the initializer expression for a local binding. Returns `None` if the binding is mutable.
/// By only considering immutable bindings, we guarantee that the returned expression represents the
/// value of the binding wherever it is referenced.
///
/// Example: For `let x = 1`, if the `HirId` of `x` is provided, the `Expr` `1` is returned.
/// Note: If you have an expression that references a binding `x`, use `path_to_local` to get the
/// canonical binding `HirId`.
pub fn find_binding_init<'tcx>(cx: &LateContext<'tcx>, hir_id: HirId) -> Option<&'tcx Expr<'tcx>> {
    let hir = cx.tcx.hir();
    if_chain! {
        if let Some(Node::Binding(pat)) = hir.find(hir_id);
        if matches!(pat.kind, PatKind::Binding(BindingAnnotation::Unannotated, ..));
        let parent = hir.get_parent_node(hir_id);
        if let Some(Node::Local(local)) = hir.find(parent);
        then {
            return local.init;
        }
    }
    None
}

/// Returns `true` if the given `NodeId` is inside a constant context
///
/// # Example
///
/// ```rust,ignore
/// if in_constant(cx, expr.hir_id) {
///     // Do something
/// }
/// ```
pub fn in_constant(cx: &LateContext<'_>, id: HirId) -> bool {
    let parent_id = cx.tcx.hir().get_parent_item(id);
    match cx.tcx.hir().get(parent_id) {
        Node::Item(&Item {
            kind: ItemKind::Const(..) | ItemKind::Static(..),
            ..
        })
        | Node::TraitItem(&TraitItem {
            kind: TraitItemKind::Const(..),
            ..
        })
        | Node::ImplItem(&ImplItem {
            kind: ImplItemKind::Const(..),
            ..
        })
        | Node::AnonConst(_) => true,
        Node::Item(&Item {
            kind: ItemKind::Fn(ref sig, ..),
            ..
        })
        | Node::ImplItem(&ImplItem {
            kind: ImplItemKind::Fn(ref sig, _),
            ..
        }) => sig.header.constness == Constness::Const,
        _ => false,
    }
}

/// Returns `true` if this `span` was expanded by any macro.
#[must_use]
pub fn in_macro(span: Span) -> bool {
    if span.from_expansion() {
        !matches!(span.ctxt().outer_expn_data().kind, ExpnKind::Desugaring(..))
    } else {
        false
    }
}

/// Checks if given pattern is a wildcard (`_`)
pub fn is_wild<'tcx>(pat: &impl std::ops::Deref<Target = Pat<'tcx>>) -> bool {
    matches!(pat.kind, PatKind::Wild)
}

/// Checks if the first type parameter is a lang item.
pub fn is_ty_param_lang_item(cx: &LateContext<'_>, qpath: &QPath<'tcx>, item: LangItem) -> Option<&'tcx hir::Ty<'tcx>> {
    let ty = get_qpath_generic_tys(qpath).next()?;

    if let TyKind::Path(qpath) = &ty.kind {
        cx.qpath_res(qpath, ty.hir_id)
            .opt_def_id()
            .map_or(false, |id| {
                cx.tcx.lang_items().require(item).map_or(false, |lang_id| id == lang_id)
            })
            .then(|| ty)
    } else {
        None
    }
}

/// Checks if the first type parameter is a diagnostic item.
pub fn is_ty_param_diagnostic_item(
    cx: &LateContext<'_>,
    qpath: &QPath<'tcx>,
    item: Symbol,
) -> Option<&'tcx hir::Ty<'tcx>> {
    let ty = get_qpath_generic_tys(qpath).next()?;

    if let TyKind::Path(qpath) = &ty.kind {
        cx.qpath_res(qpath, ty.hir_id)
            .opt_def_id()
            .map_or(false, |id| cx.tcx.is_diagnostic_item(item, id))
            .then(|| ty)
    } else {
        None
    }
}

/// Checks if the method call given in `expr` belongs to the given trait.
/// This is a deprecated function, consider using [`is_trait_method`].
pub fn match_trait_method(cx: &LateContext<'_>, expr: &Expr<'_>, path: &[&str]) -> bool {
    let def_id = cx.typeck_results().type_dependent_def_id(expr.hir_id).unwrap();
    let trt_id = cx.tcx.trait_of_item(def_id);
    trt_id.map_or(false, |trt_id| match_def_path(cx, trt_id, path))
}

/// Checks if the method call given in `def_id` belongs to a trait or other container with a given
/// diagnostic item
pub fn is_diagnostic_assoc_item(cx: &LateContext<'_>, def_id: DefId, diag_item: Symbol) -> bool {
    cx.tcx
        .opt_associated_item(def_id)
        .and_then(|associated_item| match associated_item.container {
            rustc_ty::TraitContainer(assoc_def_id) => Some(assoc_def_id),
            rustc_ty::ImplContainer(assoc_def_id) => match cx.tcx.type_of(assoc_def_id).kind() {
                rustc_ty::Adt(adt, _) => Some(adt.did),
                rustc_ty::Slice(_) => cx.tcx.get_diagnostic_item(sym::slice), // this isn't perfect but it works
                _ => None,
            },
        })
        .map_or(false, |assoc_def_id| cx.tcx.is_diagnostic_item(diag_item, assoc_def_id))
}

/// Checks if the method call given in `expr` belongs to the given trait.
pub fn is_trait_method(cx: &LateContext<'_>, expr: &Expr<'_>, diag_item: Symbol) -> bool {
    cx.typeck_results()
        .type_dependent_def_id(expr.hir_id)
        .map_or(false, |did| is_diagnostic_assoc_item(cx, did, diag_item))
}

/// Checks if an expression references a variable of the given name.
pub fn match_var(expr: &Expr<'_>, var: Symbol) -> bool {
    if let ExprKind::Path(QPath::Resolved(None, ref path)) = expr.kind {
        if let [p] = path.segments {
            return p.ident.name == var;
        }
    }
    false
}

pub fn last_path_segment<'tcx>(path: &QPath<'tcx>) -> &'tcx PathSegment<'tcx> {
    match *path {
        QPath::Resolved(_, ref path) => path.segments.last().expect("A path must have at least one segment"),
        QPath::TypeRelative(_, ref seg) => seg,
        QPath::LangItem(..) => panic!("last_path_segment: lang item has no path segments"),
    }
}

pub fn get_qpath_generics(path: &QPath<'tcx>) -> Option<&'tcx GenericArgs<'tcx>> {
    match path {
        QPath::Resolved(_, p) => p.segments.last().and_then(|s| s.args),
        QPath::TypeRelative(_, s) => s.args,
        QPath::LangItem(..) => None,
    }
}

pub fn get_qpath_generic_tys(path: &QPath<'tcx>) -> impl Iterator<Item = &'tcx hir::Ty<'tcx>> {
    get_qpath_generics(path)
        .map_or([].as_ref(), |a| a.args)
        .iter()
        .filter_map(|a| {
            if let hir::GenericArg::Type(ty) = a {
                Some(ty)
            } else {
                None
            }
        })
}

pub fn single_segment_path<'tcx>(path: &QPath<'tcx>) -> Option<&'tcx PathSegment<'tcx>> {
    match *path {
        QPath::Resolved(_, ref path) => path.segments.get(0),
        QPath::TypeRelative(_, ref seg) => Some(seg),
        QPath::LangItem(..) => None,
    }
}

/// THIS METHOD IS DEPRECATED and will eventually be removed since it does not match against the
/// entire path or resolved `DefId`. Prefer using `match_def_path`. Consider getting a `DefId` from
/// `QPath::Resolved.1.res.opt_def_id()`.
///
/// Matches a `QPath` against a slice of segment string literals.
///
/// There is also `match_path` if you are dealing with a `rustc_hir::Path` instead of a
/// `rustc_hir::QPath`.
///
/// # Examples
/// ```rust,ignore
/// match_qpath(path, &["std", "rt", "begin_unwind"])
/// ```
pub fn match_qpath(path: &QPath<'_>, segments: &[&str]) -> bool {
    match *path {
        QPath::Resolved(_, ref path) => match_path(path, segments),
        QPath::TypeRelative(ref ty, ref segment) => match ty.kind {
            TyKind::Path(ref inner_path) => {
                if let [prefix @ .., end] = segments {
                    if match_qpath(inner_path, prefix) {
                        return segment.ident.name.as_str() == *end;
                    }
                }
                false
            },
            _ => false,
        },
        QPath::LangItem(..) => false,
    }
}

/// THIS METHOD IS DEPRECATED and will eventually be removed since it does not match against the
/// entire path or resolved `DefId`. Prefer using `match_def_path`. Consider getting a `DefId` from
/// `QPath::Resolved.1.res.opt_def_id()`.
///
/// Matches a `Path` against a slice of segment string literals.
///
/// There is also `match_qpath` if you are dealing with a `rustc_hir::QPath` instead of a
/// `rustc_hir::Path`.
///
/// # Examples
///
/// ```rust,ignore
/// if match_path(&trait_ref.path, &paths::HASH) {
///     // This is the `std::hash::Hash` trait.
/// }
///
/// if match_path(ty_path, &["rustc", "lint", "Lint"]) {
///     // This is a `rustc_middle::lint::Lint`.
/// }
/// ```
pub fn match_path(path: &Path<'_>, segments: &[&str]) -> bool {
    path.segments
        .iter()
        .rev()
        .zip(segments.iter().rev())
        .all(|(a, b)| a.ident.name.as_str() == *b)
}

/// Matches a `Path` against a slice of segment string literals, e.g.
///
/// # Examples
/// ```rust,ignore
/// match_path_ast(path, &["std", "rt", "begin_unwind"])
/// ```
pub fn match_path_ast(path: &ast::Path, segments: &[&str]) -> bool {
    path.segments
        .iter()
        .rev()
        .zip(segments.iter().rev())
        .all(|(a, b)| a.ident.name.as_str() == *b)
}

/// If the expression is a path to a local, returns the canonical `HirId` of the local.
pub fn path_to_local(expr: &Expr<'_>) -> Option<HirId> {
    if let ExprKind::Path(QPath::Resolved(None, ref path)) = expr.kind {
        if let Res::Local(id) = path.res {
            return Some(id);
        }
    }
    None
}

/// Returns true if the expression is a path to a local with the specified `HirId`.
/// Use this function to see if an expression matches a function argument or a match binding.
pub fn path_to_local_id(expr: &Expr<'_>, id: HirId) -> bool {
    path_to_local(expr) == Some(id)
}

/// Gets the definition associated to a path.
#[allow(clippy::shadow_unrelated)] // false positive #6563
pub fn path_to_res(cx: &LateContext<'_>, path: &[&str]) -> Res {
    macro_rules! try_res {
        ($e:expr) => {
            match $e {
                Some(e) => e,
                None => return Res::Err,
            }
        };
    }
    fn item_child_by_name<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId, name: &str) -> Option<&'tcx Export<HirId>> {
        tcx.item_children(def_id)
            .iter()
            .find(|item| item.ident.name.as_str() == name)
    }

    let (krate, first, path) = match *path {
        [krate, first, ref path @ ..] => (krate, first, path),
        _ => return Res::Err,
    };
    let tcx = cx.tcx;
    let crates = tcx.crates();
    let krate = try_res!(crates.iter().find(|&&num| tcx.crate_name(num).as_str() == krate));
    let first = try_res!(item_child_by_name(tcx, krate.as_def_id(), first));
    let last = path
        .iter()
        .copied()
        // `get_def_path` seems to generate these empty segments for extern blocks.
        // We can just ignore them.
        .filter(|segment| !segment.is_empty())
        // for each segment, find the child item
        .try_fold(first, |item, segment| {
            let def_id = item.res.def_id();
            if let Some(item) = item_child_by_name(tcx, def_id, segment) {
                Some(item)
            } else if matches!(item.res, Res::Def(DefKind::Enum | DefKind::Struct, _)) {
                // it is not a child item so check inherent impl items
                tcx.inherent_impls(def_id)
                    .iter()
                    .find_map(|&impl_def_id| item_child_by_name(tcx, impl_def_id, segment))
            } else {
                None
            }
        });
    try_res!(last).res
}

/// Convenience function to get the `DefId` of a trait by path.
/// It could be a trait or trait alias.
pub fn get_trait_def_id(cx: &LateContext<'_>, path: &[&str]) -> Option<DefId> {
    match path_to_res(cx, path) {
        Res::Def(DefKind::Trait | DefKind::TraitAlias, trait_id) => Some(trait_id),
        _ => None,
    }
}

/// Gets the `hir::TraitRef` of the trait the given method is implemented for.
///
/// Use this if you want to find the `TraitRef` of the `Add` trait in this example:
///
/// ```rust
/// struct Point(isize, isize);
///
/// impl std::ops::Add for Point {
///     type Output = Self;
///
///     fn add(self, other: Self) -> Self {
///         Point(0, 0)
///     }
/// }
/// ```
pub fn trait_ref_of_method<'tcx>(cx: &LateContext<'tcx>, hir_id: HirId) -> Option<&'tcx TraitRef<'tcx>> {
    // Get the implemented trait for the current function
    let parent_impl = cx.tcx.hir().get_parent_item(hir_id);
    if_chain! {
        if parent_impl != hir::CRATE_HIR_ID;
        if let hir::Node::Item(item) = cx.tcx.hir().get(parent_impl);
        if let hir::ItemKind::Impl(impl_) = &item.kind;
        then { return impl_.of_trait.as_ref(); }
    }
    None
}

/// Returns the method names and argument list of nested method call expressions that make up
/// `expr`. method/span lists are sorted with the most recent call first.
pub fn method_calls<'tcx>(
    expr: &'tcx Expr<'tcx>,
    max_depth: usize,
) -> (Vec<Symbol>, Vec<&'tcx [Expr<'tcx>]>, Vec<Span>) {
    let mut method_names = Vec::with_capacity(max_depth);
    let mut arg_lists = Vec::with_capacity(max_depth);
    let mut spans = Vec::with_capacity(max_depth);

    let mut current = expr;
    for _ in 0..max_depth {
        if let ExprKind::MethodCall(path, span, args, _) = &current.kind {
            if args.iter().any(|e| e.span.from_expansion()) {
                break;
            }
            method_names.push(path.ident.name);
            arg_lists.push(&**args);
            spans.push(*span);
            current = &args[0];
        } else {
            break;
        }
    }

    (method_names, arg_lists, spans)
}

/// Matches an `Expr` against a chain of methods, and return the matched `Expr`s.
///
/// For example, if `expr` represents the `.baz()` in `foo.bar().baz()`,
/// `method_chain_args(expr, &["bar", "baz"])` will return a `Vec`
/// containing the `Expr`s for
/// `.bar()` and `.baz()`
pub fn method_chain_args<'a>(expr: &'a Expr<'_>, methods: &[&str]) -> Option<Vec<&'a [Expr<'a>]>> {
    let mut current = expr;
    let mut matched = Vec::with_capacity(methods.len());
    for method_name in methods.iter().rev() {
        // method chains are stored last -> first
        if let ExprKind::MethodCall(ref path, _, ref args, _) = current.kind {
            if path.ident.name.as_str() == *method_name {
                if args.iter().any(|e| e.span.from_expansion()) {
                    return None;
                }
                matched.push(&**args); // build up `matched` backwards
                current = &args[0] // go to parent expression
            } else {
                return None;
            }
        } else {
            return None;
        }
    }
    // Reverse `matched` so that it is in the same order as `methods`.
    matched.reverse();
    Some(matched)
}

/// Returns `true` if the provided `def_id` is an entrypoint to a program.
pub fn is_entrypoint_fn(cx: &LateContext<'_>, def_id: DefId) -> bool {
    cx.tcx
        .entry_fn(LOCAL_CRATE)
        .map_or(false, |(entry_fn_def_id, _)| def_id == entry_fn_def_id.to_def_id())
}

/// Returns `true` if the expression is in the program's `#[panic_handler]`.
pub fn is_in_panic_handler(cx: &LateContext<'_>, e: &Expr<'_>) -> bool {
    let parent = cx.tcx.hir().get_parent_item(e.hir_id);
    let def_id = cx.tcx.hir().local_def_id(parent).to_def_id();
    Some(def_id) == cx.tcx.lang_items().panic_impl()
}

/// Gets the name of the item the expression is in, if available.
pub fn get_item_name(cx: &LateContext<'_>, expr: &Expr<'_>) -> Option<Symbol> {
    let parent_id = cx.tcx.hir().get_parent_item(expr.hir_id);
    match cx.tcx.hir().find(parent_id) {
        Some(
            Node::Item(Item { ident, .. })
            | Node::TraitItem(TraitItem { ident, .. })
            | Node::ImplItem(ImplItem { ident, .. }),
        ) => Some(ident.name),
        _ => None,
    }
}

/// Gets the name of a `Pat`, if any.
pub fn get_pat_name(pat: &Pat<'_>) -> Option<Symbol> {
    match pat.kind {
        PatKind::Binding(.., ref spname, _) => Some(spname.name),
        PatKind::Path(ref qpath) => single_segment_path(qpath).map(|ps| ps.ident.name),
        PatKind::Box(ref p) | PatKind::Ref(ref p, _) => get_pat_name(&*p),
        _ => None,
    }
}

pub struct ContainsName {
    pub name: Symbol,
    pub result: bool,
}

impl<'tcx> Visitor<'tcx> for ContainsName {
    type Map = Map<'tcx>;

    fn visit_name(&mut self, _: Span, name: Symbol) {
        if self.name == name {
            self.result = true;
        }
    }
    fn nested_visit_map(&mut self) -> NestedVisitorMap<Self::Map> {
        NestedVisitorMap::None
    }
}

/// Checks if an `Expr` contains a certain name.
pub fn contains_name(name: Symbol, expr: &Expr<'_>) -> bool {
    let mut cn = ContainsName { name, result: false };
    cn.visit_expr(expr);
    cn.result
}

/// Returns `true` if `expr` contains a return expression
pub fn contains_return(expr: &hir::Expr<'_>) -> bool {
    struct RetCallFinder {
        found: bool,
    }

    impl<'tcx> hir::intravisit::Visitor<'tcx> for RetCallFinder {
        type Map = Map<'tcx>;

        fn visit_expr(&mut self, expr: &'tcx hir::Expr<'_>) {
            if self.found {
                return;
            }
            if let hir::ExprKind::Ret(..) = &expr.kind {
                self.found = true;
            } else {
                hir::intravisit::walk_expr(self, expr);
            }
        }

        fn nested_visit_map(&mut self) -> hir::intravisit::NestedVisitorMap<Self::Map> {
            hir::intravisit::NestedVisitorMap::None
        }
    }

    let mut visitor = RetCallFinder { found: false };
    visitor.visit_expr(expr);
    visitor.found
}

struct FindMacroCalls<'a, 'b> {
    names: &'a [&'b str],
    result: Vec<Span>,
}

impl<'a, 'b, 'tcx> Visitor<'tcx> for FindMacroCalls<'a, 'b> {
    type Map = Map<'tcx>;

    fn visit_expr(&mut self, expr: &'tcx Expr<'_>) {
        if self.names.iter().any(|fun| is_expn_of(expr.span, fun).is_some()) {
            self.result.push(expr.span);
        }
        // and check sub-expressions
        intravisit::walk_expr(self, expr);
    }

    fn nested_visit_map(&mut self) -> NestedVisitorMap<Self::Map> {
        NestedVisitorMap::None
    }
}

/// Finds calls of the specified macros in a function body.
pub fn find_macro_calls(names: &[&str], body: &Body<'_>) -> Vec<Span> {
    let mut fmc = FindMacroCalls {
        names,
        result: Vec::new(),
    };
    fmc.visit_expr(&body.value);
    fmc.result
}

/// Extends the span to the beginning of the spans line, incl. whitespaces.
///
/// ```rust,ignore
///        let x = ();
/// //             ^^
/// // will be converted to
///        let x = ();
/// // ^^^^^^^^^^^^^^
/// ```
fn line_span<T: LintContext>(cx: &T, span: Span) -> Span {
    let span = original_sp(span, DUMMY_SP);
    let source_map_and_line = cx.sess().source_map().lookup_line(span.lo()).unwrap();
    let line_no = source_map_and_line.line;
    let line_start = source_map_and_line.sf.lines[line_no];
    Span::new(line_start, span.hi(), span.ctxt())
}

/// Gets the parent node, if any.
pub fn get_parent_node(tcx: TyCtxt<'_>, id: HirId) -> Option<Node<'_>> {
    tcx.hir().parent_iter(id).next().map(|(_, node)| node)
}

/// Gets the parent expression, if any –- this is useful to constrain a lint.
pub fn get_parent_expr<'tcx>(cx: &LateContext<'tcx>, e: &Expr<'_>) -> Option<&'tcx Expr<'tcx>> {
    match get_parent_node(cx.tcx, e.hir_id) {
        Some(Node::Expr(parent)) => Some(parent),
        _ => None,
    }
}

pub fn get_enclosing_block<'tcx>(cx: &LateContext<'tcx>, hir_id: HirId) -> Option<&'tcx Block<'tcx>> {
    let map = &cx.tcx.hir();
    let enclosing_node = map
        .get_enclosing_scope(hir_id)
        .and_then(|enclosing_id| map.find(enclosing_id));
    enclosing_node.and_then(|node| match node {
        Node::Block(block) => Some(block),
        Node::Item(&Item {
            kind: ItemKind::Fn(_, _, eid),
            ..
        })
        | Node::ImplItem(&ImplItem {
            kind: ImplItemKind::Fn(_, eid),
            ..
        }) => match cx.tcx.hir().body(eid).value.kind {
            ExprKind::Block(ref block, _) => Some(block),
            _ => None,
        },
        _ => None,
    })
}

/// Gets the parent node if it's an impl block.
pub fn get_parent_as_impl(tcx: TyCtxt<'_>, id: HirId) -> Option<&Impl<'_>> {
    let map = tcx.hir();
    match map.parent_iter(id).next() {
        Some((
            _,
            Node::Item(Item {
                kind: ItemKind::Impl(imp),
                ..
            }),
        )) => Some(imp),
        _ => None,
    }
}

/// Checks if the given expression is the else clause of either an `if` or `if let` expression.
pub fn is_else_clause(tcx: TyCtxt<'_>, expr: &Expr<'_>) -> bool {
    let map = tcx.hir();
    let mut iter = map.parent_iter(expr.hir_id);
    match iter.next() {
        Some((arm_id, Node::Arm(..))) => matches!(
            iter.next(),
            Some((
                _,
                Node::Expr(Expr {
                    kind: ExprKind::Match(_, [_, else_arm], MatchSource::IfLetDesugar { .. }),
                    ..
                })
            ))
            if else_arm.hir_id == arm_id
        ),
        Some((
            _,
            Node::Expr(Expr {
                kind: ExprKind::If(_, _, Some(else_expr)),
                ..
            }),
        )) => else_expr.hir_id == expr.hir_id,
        _ => false,
    }
}

/// Checks whether the given expression is a constant integer of the given value.
/// unlike `is_integer_literal`, this version does const folding
pub fn is_integer_const(cx: &LateContext<'_>, e: &Expr<'_>, value: u128) -> bool {
    if is_integer_literal(e, value) {
        return true;
    }
    let map = cx.tcx.hir();
    let parent_item = map.get_parent_item(e.hir_id);
    if let Some((Constant::Int(v), _)) = map
        .maybe_body_owned_by(parent_item)
        .and_then(|body_id| constant(cx, cx.tcx.typeck_body(body_id), e))
    {
        value == v
    } else {
        false
    }
}

/// Checks whether the given expression is a constant literal of the given value.
pub fn is_integer_literal(expr: &Expr<'_>, value: u128) -> bool {
    // FIXME: use constant folding
    if let ExprKind::Lit(ref spanned) = expr.kind {
        if let LitKind::Int(v, _) = spanned.node {
            return v == value;
        }
    }
    false
}

/// Returns `true` if the given `Expr` has been coerced before.
///
/// Examples of coercions can be found in the Nomicon at
/// <https://doc.rust-lang.org/nomicon/coercions.html>.
///
/// See `rustc_middle::ty::adjustment::Adjustment` and `rustc_typeck::check::coercion` for more
/// information on adjustments and coercions.
pub fn is_adjusted(cx: &LateContext<'_>, e: &Expr<'_>) -> bool {
    cx.typeck_results().adjustments().get(e.hir_id).is_some()
}

/// Returns the pre-expansion span if is this comes from an expansion of the
/// macro `name`.
/// See also `is_direct_expn_of`.
#[must_use]
pub fn is_expn_of(mut span: Span, name: &str) -> Option<Span> {
    loop {
        if span.from_expansion() {
            let data = span.ctxt().outer_expn_data();
            let new_span = data.call_site;

            if let ExpnKind::Macro(MacroKind::Bang, mac_name) = data.kind {
                if mac_name.as_str() == name {
                    return Some(new_span);
                }
            }

            span = new_span;
        } else {
            return None;
        }
    }
}

/// Returns the pre-expansion span if the span directly comes from an expansion
/// of the macro `name`.
/// The difference with `is_expn_of` is that in
/// ```rust,ignore
/// foo!(bar!(42));
/// ```
/// `42` is considered expanded from `foo!` and `bar!` by `is_expn_of` but only
/// `bar!` by
/// `is_direct_expn_of`.
#[must_use]
pub fn is_direct_expn_of(span: Span, name: &str) -> Option<Span> {
    if span.from_expansion() {
        let data = span.ctxt().outer_expn_data();
        let new_span = data.call_site;

        if let ExpnKind::Macro(MacroKind::Bang, mac_name) = data.kind {
            if mac_name.as_str() == name {
                return Some(new_span);
            }
        }
    }

    None
}

/// Convenience function to get the return type of a function.
pub fn return_ty<'tcx>(cx: &LateContext<'tcx>, fn_item: hir::HirId) -> Ty<'tcx> {
    let fn_def_id = cx.tcx.hir().local_def_id(fn_item);
    let ret_ty = cx.tcx.fn_sig(fn_def_id).output();
    cx.tcx.erase_late_bound_regions(ret_ty)
}

/// Checks if an expression is constructing a tuple-like enum variant or struct
pub fn is_ctor_or_promotable_const_function(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    if let ExprKind::Call(ref fun, _) = expr.kind {
        if let ExprKind::Path(ref qp) = fun.kind {
            let res = cx.qpath_res(qp, fun.hir_id);
            return match res {
                def::Res::Def(DefKind::Variant | DefKind::Ctor(..), ..) => true,
                def::Res::Def(_, def_id) => cx.tcx.is_promotable_const_fn(def_id),
                _ => false,
            };
        }
    }
    false
}

/// Returns `true` if a pattern is refutable.
// TODO: should be implemented using rustc/mir_build/thir machinery
pub fn is_refutable(cx: &LateContext<'_>, pat: &Pat<'_>) -> bool {
    fn is_enum_variant(cx: &LateContext<'_>, qpath: &QPath<'_>, id: HirId) -> bool {
        matches!(
            cx.qpath_res(qpath, id),
            def::Res::Def(DefKind::Variant, ..) | Res::Def(DefKind::Ctor(def::CtorOf::Variant, _), _)
        )
    }

    fn are_refutable<'a, I: Iterator<Item = &'a Pat<'a>>>(cx: &LateContext<'_>, mut i: I) -> bool {
        i.any(|pat| is_refutable(cx, pat))
    }

    match pat.kind {
        PatKind::Wild => false,
        PatKind::Binding(_, _, _, pat) => pat.map_or(false, |pat| is_refutable(cx, pat)),
        PatKind::Box(ref pat) | PatKind::Ref(ref pat, _) => is_refutable(cx, pat),
        PatKind::Lit(..) | PatKind::Range(..) => true,
        PatKind::Path(ref qpath) => is_enum_variant(cx, qpath, pat.hir_id),
        PatKind::Or(ref pats) => {
            // TODO: should be the honest check, that pats is exhaustive set
            are_refutable(cx, pats.iter().map(|pat| &**pat))
        },
        PatKind::Tuple(ref pats, _) => are_refutable(cx, pats.iter().map(|pat| &**pat)),
        PatKind::Struct(ref qpath, ref fields, _) => {
            is_enum_variant(cx, qpath, pat.hir_id) || are_refutable(cx, fields.iter().map(|field| &*field.pat))
        },
        PatKind::TupleStruct(ref qpath, ref pats, _) => {
            is_enum_variant(cx, qpath, pat.hir_id) || are_refutable(cx, pats.iter().map(|pat| &**pat))
        },
        PatKind::Slice(ref head, ref middle, ref tail) => {
            match &cx.typeck_results().node_type(pat.hir_id).kind() {
                rustc_ty::Slice(..) => {
                    // [..] is the only irrefutable slice pattern.
                    !head.is_empty() || middle.is_none() || !tail.is_empty()
                },
                rustc_ty::Array(..) => {
                    are_refutable(cx, head.iter().chain(middle).chain(tail.iter()).map(|pat| &**pat))
                },
                _ => {
                    // unreachable!()
                    true
                },
            }
        },
    }
}

/// If the pattern is an `or` pattern, call the function once for each sub pattern. Otherwise, call
/// the function once on the given pattern.
pub fn recurse_or_patterns<'tcx, F: FnMut(&'tcx Pat<'tcx>)>(pat: &'tcx Pat<'tcx>, mut f: F) {
    if let PatKind::Or(pats) = pat.kind {
        pats.iter().cloned().for_each(f)
    } else {
        f(pat)
    }
}

/// Checks for the `#[automatically_derived]` attribute all `#[derive]`d
/// implementations have.
pub fn is_automatically_derived(attrs: &[ast::Attribute]) -> bool {
    attrs.iter().any(|attr| attr.has_name(sym::automatically_derived))
}

/// Remove blocks around an expression.
///
/// Ie. `x`, `{ x }` and `{{{{ x }}}}` all give `x`. `{ x; y }` and `{}` return
/// themselves.
pub fn remove_blocks<'tcx>(mut expr: &'tcx Expr<'tcx>) -> &'tcx Expr<'tcx> {
    while let ExprKind::Block(ref block, ..) = expr.kind {
        match (block.stmts.is_empty(), block.expr.as_ref()) {
            (true, Some(e)) => expr = e,
            _ => break,
        }
    }
    expr
}

pub fn is_self(slf: &Param<'_>) -> bool {
    if let PatKind::Binding(.., name, _) = slf.pat.kind {
        name.name == kw::SelfLower
    } else {
        false
    }
}

pub fn is_self_ty(slf: &hir::Ty<'_>) -> bool {
    if_chain! {
        if let TyKind::Path(QPath::Resolved(None, ref path)) = slf.kind;
        if let Res::SelfTy(..) = path.res;
        then {
            return true
        }
    }
    false
}

pub fn iter_input_pats<'tcx>(decl: &FnDecl<'_>, body: &'tcx Body<'_>) -> impl Iterator<Item = &'tcx Param<'tcx>> {
    (0..decl.inputs.len()).map(move |i| &body.params[i])
}

/// Checks if a given expression is a match expression expanded from the `?`
/// operator or the `try` macro.
pub fn is_try<'tcx>(expr: &'tcx Expr<'tcx>) -> Option<&'tcx Expr<'tcx>> {
    fn is_ok(arm: &Arm<'_>) -> bool {
        if_chain! {
            if let PatKind::TupleStruct(ref path, ref pat, None) = arm.pat.kind;
            if match_qpath(path, &paths::RESULT_OK[1..]);
            if let PatKind::Binding(_, hir_id, _, None) = pat[0].kind;
            if path_to_local_id(arm.body, hir_id);
            then {
                return true;
            }
        }
        false
    }

    fn is_err(arm: &Arm<'_>) -> bool {
        if let PatKind::TupleStruct(ref path, _, _) = arm.pat.kind {
            match_qpath(path, &paths::RESULT_ERR[1..])
        } else {
            false
        }
    }

    if let ExprKind::Match(_, ref arms, ref source) = expr.kind {
        // desugared from a `?` operator
        if let MatchSource::TryDesugar = *source {
            return Some(expr);
        }

        if_chain! {
            if arms.len() == 2;
            if arms[0].guard.is_none();
            if arms[1].guard.is_none();
            if (is_ok(&arms[0]) && is_err(&arms[1])) ||
                (is_ok(&arms[1]) && is_err(&arms[0]));
            then {
                return Some(expr);
            }
        }
    }

    None
}

/// Returns `true` if the lint is allowed in the current context
///
/// Useful for skipping long running code when it's unnecessary
pub fn is_allowed(cx: &LateContext<'_>, lint: &'static Lint, id: HirId) -> bool {
    cx.tcx.lint_level_at_node(lint, id).0 == Level::Allow
}

pub fn strip_pat_refs<'hir>(mut pat: &'hir Pat<'hir>) -> &'hir Pat<'hir> {
    while let PatKind::Ref(subpat, _) = pat.kind {
        pat = subpat;
    }
    pat
}

pub fn int_bits(tcx: TyCtxt<'_>, ity: rustc_ty::IntTy) -> u64 {
    Integer::from_int_ty(&tcx, ity).size().bits()
}

#[allow(clippy::cast_possible_wrap)]
/// Turn a constant int byte representation into an i128
pub fn sext(tcx: TyCtxt<'_>, u: u128, ity: rustc_ty::IntTy) -> i128 {
    let amt = 128 - int_bits(tcx, ity);
    ((u as i128) << amt) >> amt
}

#[allow(clippy::cast_sign_loss)]
/// clip unused bytes
pub fn unsext(tcx: TyCtxt<'_>, u: i128, ity: rustc_ty::IntTy) -> u128 {
    let amt = 128 - int_bits(tcx, ity);
    ((u as u128) << amt) >> amt
}

/// clip unused bytes
pub fn clip(tcx: TyCtxt<'_>, u: u128, ity: rustc_ty::UintTy) -> u128 {
    let bits = Integer::from_uint_ty(&tcx, ity).size().bits();
    let amt = 128 - bits;
    (u << amt) >> amt
}

pub fn any_parent_is_automatically_derived(tcx: TyCtxt<'_>, node: HirId) -> bool {
    let map = &tcx.hir();
    let mut prev_enclosing_node = None;
    let mut enclosing_node = node;
    while Some(enclosing_node) != prev_enclosing_node {
        if is_automatically_derived(map.attrs(enclosing_node)) {
            return true;
        }
        prev_enclosing_node = Some(enclosing_node);
        enclosing_node = map.get_parent_item(enclosing_node);
    }
    false
}

/// Matches a function call with the given path and returns the arguments.
///
/// Usage:
///
/// ```rust,ignore
/// if let Some(args) = match_function_call(cx, cmp_max_call, &paths::CMP_MAX);
/// ```
pub fn match_function_call<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'_>,
    path: &[&str],
) -> Option<&'tcx [Expr<'tcx>]> {
    if_chain! {
        if let ExprKind::Call(ref fun, ref args) = expr.kind;
        if let ExprKind::Path(ref qpath) = fun.kind;
        if let Some(fun_def_id) = cx.qpath_res(qpath, fun.hir_id).opt_def_id();
        if match_def_path(cx, fun_def_id, path);
        then {
            return Some(&args)
        }
    };
    None
}

pub fn match_def_path<'tcx>(cx: &LateContext<'tcx>, did: DefId, syms: &[&str]) -> bool {
    // We have to convert `syms` to `&[Symbol]` here because rustc's `match_def_path`
    // accepts only that. We should probably move to Symbols in Clippy as well.
    let syms = syms.iter().map(|p| Symbol::intern(p)).collect::<Vec<Symbol>>();
    cx.match_def_path(did, &syms)
}

pub fn match_panic_call<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'_>) -> Option<&'tcx [Expr<'tcx>]> {
    match_function_call(cx, expr, &paths::BEGIN_PANIC)
        .or_else(|| match_function_call(cx, expr, &paths::BEGIN_PANIC_FMT))
        .or_else(|| match_function_call(cx, expr, &paths::PANIC_ANY))
        .or_else(|| match_function_call(cx, expr, &paths::PANICKING_PANIC))
        .or_else(|| match_function_call(cx, expr, &paths::PANICKING_PANIC_FMT))
        .or_else(|| match_function_call(cx, expr, &paths::PANICKING_PANIC_STR))
}

pub fn match_panic_def_id(cx: &LateContext<'_>, did: DefId) -> bool {
    match_def_path(cx, did, &paths::BEGIN_PANIC)
        || match_def_path(cx, did, &paths::BEGIN_PANIC_FMT)
        || match_def_path(cx, did, &paths::PANIC_ANY)
        || match_def_path(cx, did, &paths::PANICKING_PANIC)
        || match_def_path(cx, did, &paths::PANICKING_PANIC_FMT)
        || match_def_path(cx, did, &paths::PANICKING_PANIC_STR)
}

/// Returns the list of condition expressions and the list of blocks in a
/// sequence of `if/else`.
/// E.g., this returns `([a, b], [c, d, e])` for the expression
/// `if a { c } else if b { d } else { e }`.
pub fn if_sequence<'tcx>(mut expr: &'tcx Expr<'tcx>) -> (Vec<&'tcx Expr<'tcx>>, Vec<&'tcx Block<'tcx>>) {
    let mut conds = Vec::new();
    let mut blocks: Vec<&Block<'_>> = Vec::new();

    while let ExprKind::If(ref cond, ref then_expr, ref else_expr) = expr.kind {
        conds.push(&**cond);
        if let ExprKind::Block(ref block, _) = then_expr.kind {
            blocks.push(block);
        } else {
            panic!("ExprKind::If node is not an ExprKind::Block");
        }

        if let Some(ref else_expr) = *else_expr {
            expr = else_expr;
        } else {
            break;
        }
    }

    // final `else {..}`
    if !blocks.is_empty() {
        if let ExprKind::Block(ref block, _) = expr.kind {
            blocks.push(&**block);
        }
    }

    (conds, blocks)
}

/// This function returns true if the given expression is the `else` or `if else` part of an if
/// statement
pub fn parent_node_is_if_expr(expr: &Expr<'_>, cx: &LateContext<'_>) -> bool {
    let map = cx.tcx.hir();
    let parent_id = map.get_parent_node(expr.hir_id);
    let parent_node = map.get(parent_id);
    matches!(
        parent_node,
        Node::Expr(Expr {
            kind: ExprKind::If(_, _, _),
            ..
        })
    )
}

// Finds the `#[must_use]` attribute, if any
pub fn must_use_attr(attrs: &[Attribute]) -> Option<&Attribute> {
    attrs.iter().find(|a| a.has_name(sym::must_use))
}

// check if expr is calling method or function with #[must_use] attribute
pub fn is_must_use_func_call(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    let did = match expr.kind {
        ExprKind::Call(ref path, _) => if_chain! {
            if let ExprKind::Path(ref qpath) = path.kind;
            if let def::Res::Def(_, did) = cx.qpath_res(qpath, path.hir_id);
            then {
                Some(did)
            } else {
                None
            }
        },
        ExprKind::MethodCall(_, _, _, _) => cx.typeck_results().type_dependent_def_id(expr.hir_id),
        _ => None,
    };

    did.map_or(false, |did| must_use_attr(&cx.tcx.get_attrs(did)).is_some())
}

pub fn is_no_std_crate(cx: &LateContext<'_>) -> bool {
    cx.tcx.hir().attrs(hir::CRATE_HIR_ID).iter().any(|attr| {
        if let ast::AttrKind::Normal(ref attr, _) = attr.kind {
            attr.path == sym::no_std
        } else {
            false
        }
    })
}

/// Check if parent of a hir node is a trait implementation block.
/// For example, `f` in
/// ```rust,ignore
/// impl Trait for S {
///     fn f() {}
/// }
/// ```
pub fn is_trait_impl_item(cx: &LateContext<'_>, hir_id: HirId) -> bool {
    if let Some(Node::Item(item)) = cx.tcx.hir().find(cx.tcx.hir().get_parent_node(hir_id)) {
        matches!(item.kind, ItemKind::Impl(hir::Impl { of_trait: Some(_), .. }))
    } else {
        false
    }
}

/// Check if it's even possible to satisfy the `where` clause for the item.
///
/// `trivial_bounds` feature allows functions with unsatisfiable bounds, for example:
///
/// ```ignore
/// fn foo() where i32: Iterator {
///     for _ in 2i32 {}
/// }
/// ```
pub fn fn_has_unsatisfiable_preds(cx: &LateContext<'_>, did: DefId) -> bool {
    use rustc_trait_selection::traits;
    let predicates = cx
        .tcx
        .predicates_of(did)
        .predicates
        .iter()
        .filter_map(|(p, _)| if p.is_global() { Some(*p) } else { None });
    traits::impossible_predicates(
        cx.tcx,
        traits::elaborate_predicates(cx.tcx, predicates)
            .map(|o| o.predicate)
            .collect::<Vec<_>>(),
    )
}

/// Returns the `DefId` of the callee if the given expression is a function or method call.
pub fn fn_def_id(cx: &LateContext<'_>, expr: &Expr<'_>) -> Option<DefId> {
    match &expr.kind {
        ExprKind::MethodCall(..) => cx.typeck_results().type_dependent_def_id(expr.hir_id),
        ExprKind::Call(
            Expr {
                kind: ExprKind::Path(qpath),
                hir_id: path_hir_id,
                ..
            },
            ..,
        ) => cx.typeck_results().qpath_res(qpath, *path_hir_id).opt_def_id(),
        _ => None,
    }
}

/// This function checks if any of the lints in the slice is enabled for the provided `HirId`.
/// A lint counts as enabled with any of the levels: `Level::Forbid` | `Level::Deny` | `Level::Warn`
///
/// ```ignore
/// #[deny(clippy::YOUR_AWESOME_LINT)]
/// println!("Hello, World!"); // <- Clippy code: run_lints(cx, &[YOUR_AWESOME_LINT], id) == true
///
/// #[allow(clippy::YOUR_AWESOME_LINT)]
/// println!("See you soon!"); // <- Clippy code: run_lints(cx, &[YOUR_AWESOME_LINT], id) == false
/// ```
pub fn run_lints(cx: &LateContext<'_>, lints: &[&'static Lint], id: HirId) -> bool {
    lints.iter().any(|lint| {
        matches!(
            cx.tcx.lint_level_at_node(lint, id),
            (Level::Forbid | Level::Deny | Level::Warn, _)
        )
    })
}

/// Returns Option<String> where String is a textual representation of the type encapsulated in the
/// slice iff the given expression is a slice of primitives (as defined in the
/// `is_recursively_primitive_type` function) and None otherwise.
pub fn is_slice_of_primitives(cx: &LateContext<'_>, expr: &Expr<'_>) -> Option<String> {
    let expr_type = cx.typeck_results().expr_ty_adjusted(expr);
    let expr_kind = expr_type.kind();
    let is_primitive = match expr_kind {
        rustc_ty::Slice(element_type) => is_recursively_primitive_type(element_type),
        rustc_ty::Ref(_, inner_ty, _) if matches!(inner_ty.kind(), &rustc_ty::Slice(_)) => {
            if let rustc_ty::Slice(element_type) = inner_ty.kind() {
                is_recursively_primitive_type(element_type)
            } else {
                unreachable!()
            }
        },
        _ => false,
    };

    if is_primitive {
        // if we have wrappers like Array, Slice or Tuple, print these
        // and get the type enclosed in the slice ref
        match expr_type.peel_refs().walk().nth(1).unwrap().expect_ty().kind() {
            rustc_ty::Slice(..) => return Some("slice".into()),
            rustc_ty::Array(..) => return Some("array".into()),
            rustc_ty::Tuple(..) => return Some("tuple".into()),
            _ => {
                // is_recursively_primitive_type() should have taken care
                // of the rest and we can rely on the type that is found
                let refs_peeled = expr_type.peel_refs();
                return Some(refs_peeled.walk().last().unwrap().to_string());
            },
        }
    }
    None
}

/// returns list of all pairs (a, b) from `exprs` such that `eq(a, b)`
/// `hash` must be comformed with `eq`
pub fn search_same<T, Hash, Eq>(exprs: &[T], hash: Hash, eq: Eq) -> Vec<(&T, &T)>
where
    Hash: Fn(&T) -> u64,
    Eq: Fn(&T, &T) -> bool,
{
    if exprs.len() == 2 && eq(&exprs[0], &exprs[1]) {
        return vec![(&exprs[0], &exprs[1])];
    }

    let mut match_expr_list: Vec<(&T, &T)> = Vec::new();

    let mut map: FxHashMap<_, Vec<&_>> =
        FxHashMap::with_capacity_and_hasher(exprs.len(), BuildHasherDefault::default());

    for expr in exprs {
        match map.entry(hash(expr)) {
            Entry::Occupied(mut o) => {
                for o in o.get() {
                    if eq(o, expr) {
                        match_expr_list.push((o, expr));
                    }
                }
                o.get_mut().push(expr);
            },
            Entry::Vacant(v) => {
                v.insert(vec![expr]);
            },
        }
    }

    match_expr_list
}

/// Peels off all references on the pattern. Returns the underlying pattern and the number of
/// references removed.
pub fn peel_hir_pat_refs(pat: &'a Pat<'a>) -> (&'a Pat<'a>, usize) {
    fn peel(pat: &'a Pat<'a>, count: usize) -> (&'a Pat<'a>, usize) {
        if let PatKind::Ref(pat, _) = pat.kind {
            peel(pat, count + 1)
        } else {
            (pat, count)
        }
    }
    peel(pat, 0)
}

/// Peels off up to the given number of references on the expression. Returns the underlying
/// expression and the number of references removed.
pub fn peel_n_hir_expr_refs(expr: &'a Expr<'a>, count: usize) -> (&'a Expr<'a>, usize) {
    fn f(expr: &'a Expr<'a>, count: usize, target: usize) -> (&'a Expr<'a>, usize) {
        match expr.kind {
            ExprKind::AddrOf(_, _, expr) if count != target => f(expr, count + 1, target),
            _ => (expr, count),
        }
    }
    f(expr, 0, count)
}

/// Peels off all references on the expression. Returns the underlying expression and the number of
/// references removed.
pub fn peel_hir_expr_refs(expr: &'a Expr<'a>) -> (&'a Expr<'a>, usize) {
    fn f(expr: &'a Expr<'a>, count: usize) -> (&'a Expr<'a>, usize) {
        match expr.kind {
            ExprKind::AddrOf(BorrowKind::Ref, _, expr) => f(expr, count + 1),
            _ => (expr, count),
        }
    }
    f(expr, 0)
}

#[macro_export]
macro_rules! unwrap_cargo_metadata {
    ($cx: ident, $lint: ident, $deps: expr) => {{
        let mut command = cargo_metadata::MetadataCommand::new();
        if !$deps {
            command.no_deps();
        }

        match command.exec() {
            Ok(metadata) => metadata,
            Err(err) => {
                span_lint($cx, $lint, DUMMY_SP, &format!("could not read cargo metadata: {}", err));
                return;
            },
        }
    }};
}

pub fn is_hir_ty_cfg_dependant(cx: &LateContext<'_>, ty: &hir::Ty<'_>) -> bool {
    if_chain! {
        if let TyKind::Path(QPath::Resolved(_, path)) = ty.kind;
        if let Res::Def(_, def_id) = path.res;
        then {
            cx.tcx.has_attr(def_id, sym::cfg) || cx.tcx.has_attr(def_id, sym::cfg_attr)
        } else {
            false
        }
    }
}

/// Check if the resolution of a given path is an `Ok` variant of `Result`.
pub fn is_ok_ctor(cx: &LateContext<'_>, res: Res) -> bool {
    if let Some(ok_id) = cx.tcx.lang_items().result_ok_variant() {
        if let Res::Def(DefKind::Ctor(CtorOf::Variant, CtorKind::Fn), id) = res {
            if let Some(variant_id) = cx.tcx.parent(id) {
                return variant_id == ok_id;
            }
        }
    }
    false
}

/// Check if the resolution of a given path is a `Some` variant of `Option`.
pub fn is_some_ctor(cx: &LateContext<'_>, res: Res) -> bool {
    if let Some(some_id) = cx.tcx.lang_items().option_some_variant() {
        if let Res::Def(DefKind::Ctor(CtorOf::Variant, CtorKind::Fn), id) = res {
            if let Some(variant_id) = cx.tcx.parent(id) {
                return variant_id == some_id;
            }
        }
    }
    false
}
