use crate::thir::cx::Cx;
use crate::thir::util::UserAnnotatedTyHelpers;
use crate::thir::*;
use rustc_data_structures::stack::ensure_sufficient_stack;
use rustc_hir as hir;
use rustc_hir::def::{CtorKind, CtorOf, DefKind, Res};
use rustc_index::vec::Idx;
use rustc_middle::hir::place::Place as HirPlace;
use rustc_middle::hir::place::PlaceBase as HirPlaceBase;
use rustc_middle::hir::place::ProjectionKind as HirProjectionKind;
use rustc_middle::mir::interpret::Scalar;
use rustc_middle::mir::BorrowKind;
use rustc_middle::ty::adjustment::{
    Adjust, Adjustment, AutoBorrow, AutoBorrowMutability, PointerCast,
};
use rustc_middle::ty::subst::{InternalSubsts, SubstsRef};
use rustc_middle::ty::{self, AdtKind, Ty};
use rustc_span::Span;

use std::iter;

impl<'thir, 'tcx> Cx<'thir, 'tcx> {
    /// Mirrors and allocates a single [`hir::Expr`]. If you need to mirror a whole slice
    /// of expressions, prefer using [`mirror_exprs`].
    ///
    /// [`mirror_exprs`]: Self::mirror_exprs
    crate fn mirror_expr(&mut self, expr: &'tcx hir::Expr<'tcx>) -> &'thir Expr<'thir, 'tcx> {
        // `mirror_expr` is recursing very deep. Make sure the stack doesn't overflow.
        ensure_sufficient_stack(|| self.arena.alloc(self.mirror_expr_inner(expr)))
    }

    /// Mirrors and allocates a slice of [`hir::Expr`]s. They will be allocated as a
    /// contiguous sequence in memory.
    crate fn mirror_exprs(&mut self, exprs: &'tcx [hir::Expr<'tcx>]) -> &'thir [Expr<'thir, 'tcx>] {
        self.arena.alloc_from_iter(exprs.iter().map(|expr| self.mirror_expr_inner(expr)))
    }

    /// Mirrors a [`hir::Expr`] without allocating it into the arena.
    /// This is a separate, private function so that [`mirror_expr`] and [`mirror_exprs`] can
    /// decide how to allocate this expression (alone or within a slice).
    ///
    /// [`mirror_expr`]: Self::mirror_expr
    /// [`mirror_exprs`]: Self::mirror_exprs
    pub(super) fn mirror_expr_inner(
        &mut self,
        hir_expr: &'tcx hir::Expr<'tcx>,
    ) -> Expr<'thir, 'tcx> {
        let temp_lifetime = self.region_scope_tree.temporary_scope(hir_expr.hir_id.local_id);
        let expr_scope =
            region::Scope { id: hir_expr.hir_id.local_id, data: region::ScopeData::Node };

        debug!("Expr::make_mirror(): id={}, span={:?}", hir_expr.hir_id, hir_expr.span);

        let mut expr = self.make_mirror_unadjusted(hir_expr);

        // Now apply adjustments, if any.
        for adjustment in self.typeck_results.expr_adjustments(hir_expr) {
            debug!("make_mirror: expr={:?} applying adjustment={:?}", expr, adjustment);
            expr = self.apply_adjustment(hir_expr, expr, adjustment);
        }

        // Next, wrap this up in the expr's scope.
        expr = Expr {
            temp_lifetime,
            ty: expr.ty,
            span: hir_expr.span,
            kind: ExprKind::Scope {
                region_scope: expr_scope,
                value: self.arena.alloc(expr),
                lint_level: LintLevel::Explicit(hir_expr.hir_id),
            },
        };

        // Finally, create a destruction scope, if any.
        if let Some(region_scope) =
            self.region_scope_tree.opt_destruction_scope(hir_expr.hir_id.local_id)
        {
            expr = Expr {
                temp_lifetime,
                ty: expr.ty,
                span: hir_expr.span,
                kind: ExprKind::Scope {
                    region_scope,
                    value: self.arena.alloc(expr),
                    lint_level: LintLevel::Inherited,
                },
            };
        }

        // OK, all done!
        expr
    }

    fn apply_adjustment(
        &mut self,
        hir_expr: &'tcx hir::Expr<'tcx>,
        mut expr: Expr<'thir, 'tcx>,
        adjustment: &Adjustment<'tcx>,
    ) -> Expr<'thir, 'tcx> {
        let Expr { temp_lifetime, mut span, .. } = expr;

        // Adjust the span from the block, to the last expression of the
        // block. This is a better span when returning a mutable reference
        // with too short a lifetime. The error message will use the span
        // from the assignment to the return place, which should only point
        // at the returned value, not the entire function body.
        //
        // fn return_short_lived<'a>(x: &'a mut i32) -> &'static mut i32 {
        //      x
        //   // ^ error message points at this expression.
        // }
        let mut adjust_span = |expr: &mut Expr<'thir, 'tcx>| {
            if let ExprKind::Block { body } = &expr.kind {
                if let Some(ref last_expr) = body.expr {
                    span = last_expr.span;
                    expr.span = span;
                }
            }
        };

        let kind = match adjustment.kind {
            Adjust::Pointer(PointerCast::Unsize) => {
                adjust_span(&mut expr);
                ExprKind::Pointer { cast: PointerCast::Unsize, source: self.arena.alloc(expr) }
            }
            Adjust::Pointer(cast) => ExprKind::Pointer { cast, source: self.arena.alloc(expr) },
            Adjust::NeverToAny => ExprKind::NeverToAny { source: self.arena.alloc(expr) },
            Adjust::Deref(None) => {
                adjust_span(&mut expr);
                ExprKind::Deref { arg: self.arena.alloc(expr) }
            }
            Adjust::Deref(Some(deref)) => {
                // We don't need to do call adjust_span here since
                // deref coercions always start with a built-in deref.
                let call = deref.method_call(self.tcx(), expr.ty);

                expr = Expr {
                    temp_lifetime,
                    ty: self
                        .tcx
                        .mk_ref(deref.region, ty::TypeAndMut { ty: expr.ty, mutbl: deref.mutbl }),
                    span,
                    kind: ExprKind::Borrow {
                        borrow_kind: deref.mutbl.to_borrow_kind(),
                        arg: self.arena.alloc(expr),
                    },
                };

                self.overloaded_place(
                    hir_expr,
                    adjustment.target,
                    Some(call),
                    self.arena.alloc_from_iter(iter::once(expr)),
                    deref.span,
                )
            }
            Adjust::Borrow(AutoBorrow::Ref(_, m)) => {
                ExprKind::Borrow { borrow_kind: m.to_borrow_kind(), arg: self.arena.alloc(expr) }
            }
            Adjust::Borrow(AutoBorrow::RawPtr(mutability)) => {
                ExprKind::AddressOf { mutability, arg: self.arena.alloc(expr) }
            }
        };

        Expr { temp_lifetime, ty: adjustment.target, span, kind }
    }

    fn make_mirror_unadjusted(&mut self, expr: &'tcx hir::Expr<'tcx>) -> Expr<'thir, 'tcx> {
        let expr_ty = self.typeck_results().expr_ty(expr);
        let temp_lifetime = self.region_scope_tree.temporary_scope(expr.hir_id.local_id);

        let kind = match expr.kind {
            // Here comes the interesting stuff:
            hir::ExprKind::MethodCall(_, method_span, ref args, fn_span) => {
                // Rewrite a.b(c) into UFCS form like Trait::b(a, c)
                let expr = self.method_callee(expr, method_span, None);
                let args = self.mirror_exprs(args);
                ExprKind::Call {
                    ty: expr.ty,
                    fun: self.arena.alloc(expr),
                    args,
                    from_hir_call: true,
                    fn_span,
                }
            }

            hir::ExprKind::Call(ref fun, ref args) => {
                if self.typeck_results().is_method_call(expr) {
                    // The callee is something implementing Fn, FnMut, or FnOnce.
                    // Find the actual method implementation being called and
                    // build the appropriate UFCS call expression with the
                    // callee-object as expr parameter.

                    // rewrite f(u, v) into FnOnce::call_once(f, (u, v))

                    let method = self.method_callee(expr, fun.span, None);

                    let arg_tys = args.iter().map(|e| self.typeck_results().expr_ty_adjusted(e));
                    let tupled_args = Expr {
                        ty: self.tcx.mk_tup(arg_tys),
                        temp_lifetime,
                        span: expr.span,
                        kind: ExprKind::Tuple { fields: self.mirror_exprs(args) },
                    };

                    ExprKind::Call {
                        ty: method.ty,
                        fun: self.arena.alloc(method),
                        args: self
                            .arena
                            .alloc_from_iter(vec![self.mirror_expr_inner(fun), tupled_args]),
                        from_hir_call: true,
                        fn_span: expr.span,
                    }
                } else {
                    let adt_data =
                        if let hir::ExprKind::Path(hir::QPath::Resolved(_, ref path)) = fun.kind {
                            // Tuple-like ADTs are represented as ExprKind::Call. We convert them here.
                            expr_ty.ty_adt_def().and_then(|adt_def| match path.res {
                                Res::Def(DefKind::Ctor(_, CtorKind::Fn), ctor_id) => {
                                    Some((adt_def, adt_def.variant_index_with_ctor_id(ctor_id)))
                                }
                                Res::SelfCtor(..) => Some((adt_def, VariantIdx::new(0))),
                                _ => None,
                            })
                        } else {
                            None
                        };
                    if let Some((adt_def, index)) = adt_data {
                        let substs = self.typeck_results().node_substs(fun.hir_id);
                        let user_provided_types = self.typeck_results().user_provided_types();
                        let user_ty =
                            user_provided_types.get(fun.hir_id).copied().map(|mut u_ty| {
                                if let UserType::TypeOf(ref mut did, _) = &mut u_ty.value {
                                    *did = adt_def.did;
                                }
                                u_ty
                            });
                        debug!("make_mirror_unadjusted: (call) user_ty={:?}", user_ty);

                        let field_refs =
                            self.arena.alloc_from_iter(args.iter().enumerate().map(|(idx, e)| {
                                FieldExpr { name: Field::new(idx), expr: self.mirror_expr(e) }
                            }));
                        ExprKind::Adt {
                            adt_def,
                            substs,
                            variant_index: index,
                            fields: field_refs,
                            user_ty,
                            base: None,
                        }
                    } else {
                        ExprKind::Call {
                            ty: self.typeck_results().node_type(fun.hir_id),
                            fun: self.mirror_expr(fun),
                            args: self.mirror_exprs(args),
                            from_hir_call: true,
                            fn_span: expr.span,
                        }
                    }
                }
            }

            hir::ExprKind::AddrOf(hir::BorrowKind::Ref, mutbl, ref arg) => {
                ExprKind::Borrow { borrow_kind: mutbl.to_borrow_kind(), arg: self.mirror_expr(arg) }
            }

            hir::ExprKind::AddrOf(hir::BorrowKind::Raw, mutability, ref arg) => {
                ExprKind::AddressOf { mutability, arg: self.mirror_expr(arg) }
            }

            hir::ExprKind::Block(ref blk, _) => ExprKind::Block { body: self.mirror_block(blk) },

            hir::ExprKind::Assign(ref lhs, ref rhs, _) => {
                ExprKind::Assign { lhs: self.mirror_expr(lhs), rhs: self.mirror_expr(rhs) }
            }

            hir::ExprKind::AssignOp(op, ref lhs, ref rhs) => {
                if self.typeck_results().is_method_call(expr) {
                    let lhs = self.mirror_expr_inner(lhs);
                    let rhs = self.mirror_expr_inner(rhs);
                    self.overloaded_operator(expr, self.arena.alloc_from_iter(vec![lhs, rhs]))
                } else {
                    ExprKind::AssignOp {
                        op: bin_op(op.node),
                        lhs: self.mirror_expr(lhs),
                        rhs: self.mirror_expr(rhs),
                    }
                }
            }

            hir::ExprKind::Lit(ref lit) => ExprKind::Literal {
                literal: self.const_eval_literal(&lit.node, expr_ty, lit.span, false),
                user_ty: None,
                const_id: None,
            },

            hir::ExprKind::Binary(op, ref lhs, ref rhs) => {
                if self.typeck_results().is_method_call(expr) {
                    let lhs = self.mirror_expr_inner(lhs);
                    let rhs = self.mirror_expr_inner(rhs);
                    self.overloaded_operator(expr, self.arena.alloc_from_iter(vec![lhs, rhs]))
                } else {
                    // FIXME overflow
                    match op.node {
                        hir::BinOpKind::And => ExprKind::LogicalOp {
                            op: LogicalOp::And,
                            lhs: self.mirror_expr(lhs),
                            rhs: self.mirror_expr(rhs),
                        },
                        hir::BinOpKind::Or => ExprKind::LogicalOp {
                            op: LogicalOp::Or,
                            lhs: self.mirror_expr(lhs),
                            rhs: self.mirror_expr(rhs),
                        },

                        _ => {
                            let op = bin_op(op.node);
                            ExprKind::Binary {
                                op,
                                lhs: self.mirror_expr(lhs),
                                rhs: self.mirror_expr(rhs),
                            }
                        }
                    }
                }
            }

            hir::ExprKind::Index(ref lhs, ref index) => {
                if self.typeck_results().is_method_call(expr) {
                    let lhs = self.mirror_expr_inner(lhs);
                    let index = self.mirror_expr_inner(index);
                    self.overloaded_place(
                        expr,
                        expr_ty,
                        None,
                        self.arena.alloc_from_iter(vec![lhs, index]),
                        expr.span,
                    )
                } else {
                    ExprKind::Index { lhs: self.mirror_expr(lhs), index: self.mirror_expr(index) }
                }
            }

            hir::ExprKind::Unary(hir::UnOp::Deref, ref arg) => {
                if self.typeck_results().is_method_call(expr) {
                    let arg = self.mirror_expr_inner(arg);
                    self.overloaded_place(
                        expr,
                        expr_ty,
                        None,
                        self.arena.alloc_from_iter(iter::once(arg)),
                        expr.span,
                    )
                } else {
                    ExprKind::Deref { arg: self.mirror_expr(arg) }
                }
            }

            hir::ExprKind::Unary(hir::UnOp::Not, ref arg) => {
                if self.typeck_results().is_method_call(expr) {
                    let arg = self.mirror_expr_inner(arg);
                    self.overloaded_operator(expr, self.arena.alloc_from_iter(iter::once(arg)))
                } else {
                    ExprKind::Unary { op: UnOp::Not, arg: self.mirror_expr(arg) }
                }
            }

            hir::ExprKind::Unary(hir::UnOp::Neg, ref arg) => {
                if self.typeck_results().is_method_call(expr) {
                    let arg = self.mirror_expr_inner(arg);
                    self.overloaded_operator(expr, self.arena.alloc_from_iter(iter::once(arg)))
                } else if let hir::ExprKind::Lit(ref lit) = arg.kind {
                    ExprKind::Literal {
                        literal: self.const_eval_literal(&lit.node, expr_ty, lit.span, true),
                        user_ty: None,
                        const_id: None,
                    }
                } else {
                    ExprKind::Unary { op: UnOp::Neg, arg: self.mirror_expr(arg) }
                }
            }

            hir::ExprKind::Struct(ref qpath, ref fields, ref base) => match expr_ty.kind() {
                ty::Adt(adt, substs) => match adt.adt_kind() {
                    AdtKind::Struct | AdtKind::Union => {
                        let user_provided_types = self.typeck_results().user_provided_types();
                        let user_ty = user_provided_types.get(expr.hir_id).copied();
                        debug!("make_mirror_unadjusted: (struct/union) user_ty={:?}", user_ty);
                        ExprKind::Adt {
                            adt_def: adt,
                            variant_index: VariantIdx::new(0),
                            substs,
                            user_ty,
                            fields: self.field_refs(fields),
                            base: base.as_ref().map(|base| FruInfo {
                                base: self.mirror_expr(base),
                                field_types: self.arena.alloc_from_iter(
                                    self.typeck_results().fru_field_types()[expr.hir_id]
                                        .iter()
                                        .cloned(),
                                ),
                            }),
                        }
                    }
                    AdtKind::Enum => {
                        let res = self.typeck_results().qpath_res(qpath, expr.hir_id);
                        match res {
                            Res::Def(DefKind::Variant, variant_id) => {
                                assert!(base.is_none());

                                let index = adt.variant_index_with_id(variant_id);
                                let user_provided_types =
                                    self.typeck_results().user_provided_types();
                                let user_ty = user_provided_types.get(expr.hir_id).copied();
                                debug!("make_mirror_unadjusted: (variant) user_ty={:?}", user_ty);
                                ExprKind::Adt {
                                    adt_def: adt,
                                    variant_index: index,
                                    substs,
                                    user_ty,
                                    fields: self.field_refs(fields),
                                    base: None,
                                }
                            }
                            _ => {
                                span_bug!(expr.span, "unexpected res: {:?}", res);
                            }
                        }
                    }
                },
                _ => {
                    span_bug!(expr.span, "unexpected type for struct literal: {:?}", expr_ty);
                }
            },

            hir::ExprKind::Closure(..) => {
                let closure_ty = self.typeck_results().expr_ty(expr);
                let (def_id, substs, movability) = match *closure_ty.kind() {
                    ty::Closure(def_id, substs) => (def_id, UpvarSubsts::Closure(substs), None),
                    ty::Generator(def_id, substs, movability) => {
                        (def_id, UpvarSubsts::Generator(substs), Some(movability))
                    }
                    _ => {
                        span_bug!(expr.span, "closure expr w/o closure type: {:?}", closure_ty);
                    }
                };

                let upvars = self.arena.alloc_from_iter(
                    self.typeck_results
                        .closure_min_captures_flattened(def_id)
                        .zip(substs.upvar_tys())
                        .map(|(captured_place, ty)| self.capture_upvar(expr, captured_place, ty)),
                );

                // Convert the closure fake reads, if any, from hir `Place` to ExprRef
                let fake_reads = match self.typeck_results.closure_fake_reads.get(&def_id) {
                    Some(fake_reads) => fake_reads
                        .iter()
                        .map(|(place, cause, hir_id)| {
                            let expr = self.convert_captured_hir_place(expr, place.clone());
                            let expr_ref: &'thir Expr<'thir, 'tcx> = self.arena.alloc(expr);
                            (expr_ref, *cause, *hir_id)
                        })
                        .collect(),
                    None => Vec::new(),
                };

                ExprKind::Closure { closure_id: def_id, substs, upvars, movability, fake_reads }
            }

            hir::ExprKind::Path(ref qpath) => {
                let res = self.typeck_results().qpath_res(qpath, expr.hir_id);
                self.convert_path_expr(expr, res)
            }

            hir::ExprKind::InlineAsm(ref asm) => ExprKind::InlineAsm {
                template: asm.template,
                operands: self.arena.alloc_from_iter(asm.operands.iter().map(|(op, _op_sp)| {
                    match *op {
                        hir::InlineAsmOperand::In { reg, ref expr } => {
                            InlineAsmOperand::In { reg, expr: self.mirror_expr(expr) }
                        }
                        hir::InlineAsmOperand::Out { reg, late, ref expr } => {
                            InlineAsmOperand::Out {
                                reg,
                                late,
                                expr: expr.as_ref().map(|expr| self.mirror_expr(expr)),
                            }
                        }
                        hir::InlineAsmOperand::InOut { reg, late, ref expr } => {
                            InlineAsmOperand::InOut { reg, late, expr: self.mirror_expr(expr) }
                        }
                        hir::InlineAsmOperand::SplitInOut {
                            reg,
                            late,
                            ref in_expr,
                            ref out_expr,
                        } => InlineAsmOperand::SplitInOut {
                            reg,
                            late,
                            in_expr: self.mirror_expr(in_expr),
                            out_expr: out_expr.as_ref().map(|expr| self.mirror_expr(expr)),
                        },
                        hir::InlineAsmOperand::Const { ref anon_const } => {
                            let anon_const_def_id = self.tcx.hir().local_def_id(anon_const.hir_id);
                            let value = ty::Const::from_anon_const(self.tcx, anon_const_def_id);
                            let span = self.tcx.hir().span(anon_const.hir_id);

                            InlineAsmOperand::Const { value, span }
                        }
                        hir::InlineAsmOperand::Sym { ref expr } => {
                            let qpath = match expr.kind {
                                hir::ExprKind::Path(ref qpath) => qpath,
                                _ => span_bug!(
                                    expr.span,
                                    "asm `sym` operand should be a path, found {:?}",
                                    expr.kind
                                ),
                            };
                            let temp_lifetime =
                                self.region_scope_tree.temporary_scope(expr.hir_id.local_id);
                            let res = self.typeck_results().qpath_res(qpath, expr.hir_id);
                            let ty;
                            match res {
                                Res::Def(DefKind::Fn, _) | Res::Def(DefKind::AssocFn, _) => {
                                    ty = self.typeck_results().node_type(expr.hir_id);
                                    let user_ty = self.user_substs_applied_to_res(expr.hir_id, res);
                                    InlineAsmOperand::SymFn {
                                        expr: self.arena.alloc(Expr {
                                            ty,
                                            temp_lifetime,
                                            span: expr.span,
                                            kind: ExprKind::Literal {
                                                literal: ty::Const::zero_sized(self.tcx, ty),
                                                user_ty,
                                                const_id: None,
                                            },
                                        }),
                                    }
                                }

                                Res::Def(DefKind::Static, def_id) => {
                                    InlineAsmOperand::SymStatic { def_id }
                                }

                                _ => {
                                    self.tcx.sess.span_err(
                                        expr.span,
                                        "asm `sym` operand must point to a fn or static",
                                    );

                                    // Not a real fn, but we're not reaching codegen anyways...
                                    ty = self.tcx.ty_error();
                                    InlineAsmOperand::SymFn {
                                        expr: self.arena.alloc(Expr {
                                            ty,
                                            temp_lifetime,
                                            span: expr.span,
                                            kind: ExprKind::Literal {
                                                literal: ty::Const::zero_sized(self.tcx, ty),
                                                user_ty: None,
                                                const_id: None,
                                            },
                                        }),
                                    }
                                }
                            }
                        }
                    }
                })),
                options: asm.options,
                line_spans: asm.line_spans,
            },

            hir::ExprKind::LlvmInlineAsm(ref asm) => ExprKind::LlvmInlineAsm {
                asm: &asm.inner,
                outputs: self.mirror_exprs(asm.outputs_exprs),
                inputs: self.mirror_exprs(asm.inputs_exprs),
            },

            hir::ExprKind::ConstBlock(ref anon_const) => {
                let anon_const_def_id = self.tcx.hir().local_def_id(anon_const.hir_id);
                let value = ty::Const::from_anon_const(self.tcx, anon_const_def_id);

                ExprKind::ConstBlock { value }
            }
            // Now comes the rote stuff:
            hir::ExprKind::Repeat(ref v, ref count) => {
                let count_def_id = self.tcx.hir().local_def_id(count.hir_id);
                let count = ty::Const::from_anon_const(self.tcx, count_def_id);

                ExprKind::Repeat { value: self.mirror_expr(v), count }
            }
            hir::ExprKind::Ret(ref v) => {
                ExprKind::Return { value: v.as_ref().map(|v| self.mirror_expr(v)) }
            }
            hir::ExprKind::Break(dest, ref value) => match dest.target_id {
                Ok(target_id) => ExprKind::Break {
                    label: region::Scope { id: target_id.local_id, data: region::ScopeData::Node },
                    value: value.as_ref().map(|value| self.mirror_expr(value)),
                },
                Err(err) => bug!("invalid loop id for break: {}", err),
            },
            hir::ExprKind::Continue(dest) => match dest.target_id {
                Ok(loop_id) => ExprKind::Continue {
                    label: region::Scope { id: loop_id.local_id, data: region::ScopeData::Node },
                },
                Err(err) => bug!("invalid loop id for continue: {}", err),
            },
            hir::ExprKind::If(cond, then, else_opt) => ExprKind::If {
                cond: self.mirror_expr(cond),
                then: self.mirror_expr(then),
                else_opt: else_opt.map(|el| self.mirror_expr(el)),
            },
            hir::ExprKind::Match(ref discr, ref arms, _) => ExprKind::Match {
                scrutinee: self.mirror_expr(discr),
                arms: self.arena.alloc_from_iter(arms.iter().map(|a| self.convert_arm(a))),
            },
            hir::ExprKind::Loop(ref body, ..) => {
                let block_ty = self.typeck_results().node_type(body.hir_id);
                let temp_lifetime = self.region_scope_tree.temporary_scope(body.hir_id.local_id);
                let block = self.mirror_block(body);
                let body = self.arena.alloc(Expr {
                    ty: block_ty,
                    temp_lifetime,
                    span: block.span,
                    kind: ExprKind::Block { body: block },
                });
                ExprKind::Loop { body }
            }
            hir::ExprKind::Field(ref source, ..) => ExprKind::Field {
                lhs: self.mirror_expr(source),
                name: Field::new(self.tcx.field_index(expr.hir_id, self.typeck_results)),
            },
            hir::ExprKind::Cast(ref source, ref cast_ty) => {
                // Check for a user-given type annotation on this `cast`
                let user_provided_types = self.typeck_results.user_provided_types();
                let user_ty = user_provided_types.get(cast_ty.hir_id);

                debug!(
                    "cast({:?}) has ty w/ hir_id {:?} and user provided ty {:?}",
                    expr, cast_ty.hir_id, user_ty,
                );

                // Check to see if this cast is a "coercion cast", where the cast is actually done
                // using a coercion (or is a no-op).
                let cast = if self.typeck_results().is_coercion_cast(source.hir_id) {
                    // Convert the lexpr to a vexpr.
                    ExprKind::Use { source: self.mirror_expr(source) }
                } else if self.typeck_results().expr_ty(source).is_region_ptr() {
                    // Special cased so that we can type check that the element
                    // type of the source matches the pointed to type of the
                    // destination.
                    ExprKind::Pointer {
                        source: self.mirror_expr(source),
                        cast: PointerCast::ArrayToPointer,
                    }
                } else {
                    // check whether this is casting an enum variant discriminant
                    // to prevent cycles, we refer to the discriminant initializer
                    // which is always an integer and thus doesn't need to know the
                    // enum's layout (or its tag type) to compute it during const eval
                    // Example:
                    // enum Foo {
                    //     A,
                    //     B = A as isize + 4,
                    // }
                    // The correct solution would be to add symbolic computations to miri,
                    // so we wouldn't have to compute and store the actual value
                    let var = if let hir::ExprKind::Path(ref qpath) = source.kind {
                        let res = self.typeck_results().qpath_res(qpath, source.hir_id);
                        self.typeck_results().node_type(source.hir_id).ty_adt_def().and_then(
                            |adt_def| match res {
                                Res::Def(
                                    DefKind::Ctor(CtorOf::Variant, CtorKind::Const),
                                    variant_ctor_id,
                                ) => {
                                    let idx = adt_def.variant_index_with_ctor_id(variant_ctor_id);
                                    let (d, o) = adt_def.discriminant_def_for_variant(idx);
                                    use rustc_middle::ty::util::IntTypeExt;
                                    let ty = adt_def.repr.discr_type();
                                    let ty = ty.to_ty(self.tcx());
                                    Some((d, o, ty))
                                }
                                _ => None,
                            },
                        )
                    } else {
                        None
                    };

                    let source = if let Some((did, offset, var_ty)) = var {
                        let mk_const = |literal| {
                            self.arena.alloc(Expr {
                                temp_lifetime,
                                ty: var_ty,
                                span: expr.span,
                                kind: ExprKind::Literal { literal, user_ty: None, const_id: None },
                            })
                        };
                        let offset = mk_const(ty::Const::from_bits(
                            self.tcx,
                            offset as u128,
                            self.param_env.and(var_ty),
                        ));
                        match did {
                            Some(did) => {
                                // in case we are offsetting from a computed discriminant
                                // and not the beginning of discriminants (which is always `0`)
                                let substs = InternalSubsts::identity_for_item(self.tcx(), did);
                                let lhs = mk_const(self.tcx().mk_const(ty::Const {
                                    val: ty::ConstKind::Unevaluated(ty::Unevaluated {
                                        def: ty::WithOptConstParam::unknown(did),
                                        substs,
                                        promoted: None,
                                    }),
                                    ty: var_ty,
                                }));
                                let bin =
                                    ExprKind::Binary { op: BinOp::Add, lhs: lhs, rhs: offset };
                                self.arena.alloc(Expr {
                                    temp_lifetime,
                                    ty: var_ty,
                                    span: expr.span,
                                    kind: bin,
                                })
                            }
                            None => offset,
                        }
                    } else {
                        self.mirror_expr(source)
                    };

                    ExprKind::Cast { source: source }
                };

                if let Some(user_ty) = user_ty {
                    // NOTE: Creating a new Expr and wrapping a Cast inside of it may be
                    //       inefficient, revisit this when performance becomes an issue.
                    let cast_expr = self.arena.alloc(Expr {
                        temp_lifetime,
                        ty: expr_ty,
                        span: expr.span,
                        kind: cast,
                    });
                    debug!("make_mirror_unadjusted: (cast) user_ty={:?}", user_ty);

                    ExprKind::ValueTypeAscription { source: cast_expr, user_ty: Some(*user_ty) }
                } else {
                    cast
                }
            }
            hir::ExprKind::Type(ref source, ref ty) => {
                let user_provided_types = self.typeck_results.user_provided_types();
                let user_ty = user_provided_types.get(ty.hir_id).copied();
                debug!("make_mirror_unadjusted: (type) user_ty={:?}", user_ty);
                let mirrored = self.mirror_expr(source);
                if source.is_syntactic_place_expr() {
                    ExprKind::PlaceTypeAscription { source: mirrored, user_ty }
                } else {
                    ExprKind::ValueTypeAscription { source: mirrored, user_ty }
                }
            }
            hir::ExprKind::DropTemps(ref source) => {
                ExprKind::Use { source: self.mirror_expr(source) }
            }
            hir::ExprKind::Box(ref value) => ExprKind::Box { value: self.mirror_expr(value) },
            hir::ExprKind::Array(ref fields) => {
                ExprKind::Array { fields: self.mirror_exprs(fields) }
            }
            hir::ExprKind::Tup(ref fields) => ExprKind::Tuple { fields: self.mirror_exprs(fields) },

            hir::ExprKind::Yield(ref v, _) => ExprKind::Yield { value: self.mirror_expr(v) },
            hir::ExprKind::Err => unreachable!(),
        };

        Expr { temp_lifetime, ty: expr_ty, span: expr.span, kind }
    }

    fn user_substs_applied_to_res(
        &mut self,
        hir_id: hir::HirId,
        res: Res,
    ) -> Option<ty::CanonicalUserType<'tcx>> {
        debug!("user_substs_applied_to_res: res={:?}", res);
        let user_provided_type = match res {
            // A reference to something callable -- e.g., a fn, method, or
            // a tuple-struct or tuple-variant. This has the type of a
            // `Fn` but with the user-given substitutions.
            Res::Def(DefKind::Fn, _)
            | Res::Def(DefKind::AssocFn, _)
            | Res::Def(DefKind::Ctor(_, CtorKind::Fn), _)
            | Res::Def(DefKind::Const, _)
            | Res::Def(DefKind::AssocConst, _) => {
                self.typeck_results().user_provided_types().get(hir_id).copied()
            }

            // A unit struct/variant which is used as a value (e.g.,
            // `None`). This has the type of the enum/struct that defines
            // this variant -- but with the substitutions given by the
            // user.
            Res::Def(DefKind::Ctor(_, CtorKind::Const), _) => {
                self.user_substs_applied_to_ty_of_hir_id(hir_id)
            }

            // `Self` is used in expression as a tuple struct constructor or an unit struct constructor
            Res::SelfCtor(_) => self.user_substs_applied_to_ty_of_hir_id(hir_id),

            _ => bug!("user_substs_applied_to_res: unexpected res {:?} at {:?}", res, hir_id),
        };
        debug!("user_substs_applied_to_res: user_provided_type={:?}", user_provided_type);
        user_provided_type
    }

    fn method_callee(
        &mut self,
        expr: &hir::Expr<'_>,
        span: Span,
        overloaded_callee: Option<(DefId, SubstsRef<'tcx>)>,
    ) -> Expr<'thir, 'tcx> {
        let temp_lifetime = self.region_scope_tree.temporary_scope(expr.hir_id.local_id);
        let (def_id, substs, user_ty) = match overloaded_callee {
            Some((def_id, substs)) => (def_id, substs, None),
            None => {
                let (kind, def_id) =
                    self.typeck_results().type_dependent_def(expr.hir_id).unwrap_or_else(|| {
                        span_bug!(expr.span, "no type-dependent def for method callee")
                    });
                let user_ty = self.user_substs_applied_to_res(expr.hir_id, Res::Def(kind, def_id));
                debug!("method_callee: user_ty={:?}", user_ty);
                (def_id, self.typeck_results().node_substs(expr.hir_id), user_ty)
            }
        };
        let ty = self.tcx().mk_fn_def(def_id, substs);
        Expr {
            temp_lifetime,
            ty,
            span,
            kind: ExprKind::Literal {
                literal: ty::Const::zero_sized(self.tcx(), ty),
                user_ty,
                const_id: None,
            },
        }
    }

    fn convert_arm(&mut self, arm: &'tcx hir::Arm<'tcx>) -> Arm<'thir, 'tcx> {
        Arm {
            pattern: self.pattern_from_hir(&arm.pat),
            guard: arm.guard.as_ref().map(|g| match g {
                hir::Guard::If(ref e) => Guard::If(self.mirror_expr(e)),
                hir::Guard::IfLet(ref pat, ref e) => {
                    Guard::IfLet(self.pattern_from_hir(pat), self.mirror_expr(e))
                }
            }),
            body: self.mirror_expr(arm.body),
            lint_level: LintLevel::Explicit(arm.hir_id),
            scope: region::Scope { id: arm.hir_id.local_id, data: region::ScopeData::Node },
            span: arm.span,
        }
    }

    fn convert_path_expr(
        &mut self,
        expr: &'tcx hir::Expr<'tcx>,
        res: Res,
    ) -> ExprKind<'thir, 'tcx> {
        let substs = self.typeck_results().node_substs(expr.hir_id);
        match res {
            // A regular function, constructor function or a constant.
            Res::Def(DefKind::Fn, _)
            | Res::Def(DefKind::AssocFn, _)
            | Res::Def(DefKind::Ctor(_, CtorKind::Fn), _)
            | Res::SelfCtor(..) => {
                let user_ty = self.user_substs_applied_to_res(expr.hir_id, res);
                debug!("convert_path_expr: user_ty={:?}", user_ty);
                ExprKind::Literal {
                    literal: ty::Const::zero_sized(
                        self.tcx,
                        self.typeck_results().node_type(expr.hir_id),
                    ),
                    user_ty,
                    const_id: None,
                }
            }

            Res::Def(DefKind::ConstParam, def_id) => {
                let hir_id = self.tcx.hir().local_def_id_to_hir_id(def_id.expect_local());
                let item_id = self.tcx.hir().get_parent_node(hir_id);
                let item_def_id = self.tcx.hir().local_def_id(item_id);
                let generics = self.tcx.generics_of(item_def_id);
                let index = generics.param_def_id_to_index[&def_id];
                let name = self.tcx.hir().name(hir_id);
                let val = ty::ConstKind::Param(ty::ParamConst::new(index, name));
                ExprKind::Literal {
                    literal: self.tcx.mk_const(ty::Const {
                        val,
                        ty: self.typeck_results().node_type(expr.hir_id),
                    }),
                    user_ty: None,
                    const_id: Some(def_id),
                }
            }

            Res::Def(DefKind::Const, def_id) | Res::Def(DefKind::AssocConst, def_id) => {
                let user_ty = self.user_substs_applied_to_res(expr.hir_id, res);
                debug!("convert_path_expr: (const) user_ty={:?}", user_ty);
                ExprKind::Literal {
                    literal: self.tcx.mk_const(ty::Const {
                        val: ty::ConstKind::Unevaluated(ty::Unevaluated {
                            def: ty::WithOptConstParam::unknown(def_id),
                            substs,
                            promoted: None,
                        }),
                        ty: self.typeck_results().node_type(expr.hir_id),
                    }),
                    user_ty,
                    const_id: Some(def_id),
                }
            }

            Res::Def(DefKind::Ctor(_, CtorKind::Const), def_id) => {
                let user_provided_types = self.typeck_results.user_provided_types();
                let user_provided_type = user_provided_types.get(expr.hir_id).copied();
                debug!("convert_path_expr: user_provided_type={:?}", user_provided_type);
                let ty = self.typeck_results().node_type(expr.hir_id);
                match ty.kind() {
                    // A unit struct/variant which is used as a value.
                    // We return a completely different ExprKind here to account for this special case.
                    ty::Adt(adt_def, substs) => ExprKind::Adt {
                        adt_def,
                        variant_index: adt_def.variant_index_with_ctor_id(def_id),
                        substs,
                        user_ty: user_provided_type,
                        fields: self.arena.alloc_from_iter(iter::empty()),
                        base: None,
                    },
                    _ => bug!("unexpected ty: {:?}", ty),
                }
            }

            // We encode uses of statics as a `*&STATIC` where the `&STATIC` part is
            // a constant reference (or constant raw pointer for `static mut`) in MIR
            Res::Def(DefKind::Static, id) => {
                let ty = self.tcx.static_ptr_ty(id);
                let temp_lifetime = self.region_scope_tree.temporary_scope(expr.hir_id.local_id);
                let kind = if self.tcx.is_thread_local_static(id) {
                    ExprKind::ThreadLocalRef(id)
                } else {
                    let ptr = self.tcx.create_static_alloc(id);
                    ExprKind::StaticRef {
                        literal: ty::Const::from_scalar(self.tcx, Scalar::Ptr(ptr.into()), ty),
                        def_id: id,
                    }
                };
                ExprKind::Deref {
                    arg: self.arena.alloc(Expr { ty, temp_lifetime, span: expr.span, kind }),
                }
            }

            Res::Local(var_hir_id) => self.convert_var(var_hir_id),

            _ => span_bug!(expr.span, "res `{:?}` not yet implemented", res),
        }
    }

    fn convert_var(&mut self, var_hir_id: hir::HirId) -> ExprKind<'thir, 'tcx> {
        // We want upvars here not captures.
        // Captures will be handled in MIR.
        let is_upvar = self
            .tcx
            .upvars_mentioned(self.body_owner)
            .map_or(false, |upvars| upvars.contains_key(&var_hir_id));

        debug!(
            "convert_var({:?}): is_upvar={}, body_owner={:?}",
            var_hir_id, is_upvar, self.body_owner
        );

        if is_upvar {
            ExprKind::UpvarRef { closure_def_id: self.body_owner, var_hir_id }
        } else {
            ExprKind::VarRef { id: var_hir_id }
        }
    }

    fn overloaded_operator(
        &mut self,
        expr: &'tcx hir::Expr<'tcx>,
        args: &'thir [Expr<'thir, 'tcx>],
    ) -> ExprKind<'thir, 'tcx> {
        let fun = self.arena.alloc(self.method_callee(expr, expr.span, None));
        ExprKind::Call { ty: fun.ty, fun, args, from_hir_call: false, fn_span: expr.span }
    }

    fn overloaded_place(
        &mut self,
        expr: &'tcx hir::Expr<'tcx>,
        place_ty: Ty<'tcx>,
        overloaded_callee: Option<(DefId, SubstsRef<'tcx>)>,
        args: &'thir [Expr<'thir, 'tcx>],
        span: Span,
    ) -> ExprKind<'thir, 'tcx> {
        // For an overloaded *x or x[y] expression of type T, the method
        // call returns an &T and we must add the deref so that the types
        // line up (this is because `*x` and `x[y]` represent places):

        // Reconstruct the output assuming it's a reference with the
        // same region and mutability as the receiver. This holds for
        // `Deref(Mut)::Deref(_mut)` and `Index(Mut)::index(_mut)`.
        let (region, mutbl) = match *args[0].ty.kind() {
            ty::Ref(region, _, mutbl) => (region, mutbl),
            _ => span_bug!(span, "overloaded_place: receiver is not a reference"),
        };
        let ref_ty = self.tcx.mk_ref(region, ty::TypeAndMut { ty: place_ty, mutbl });

        // construct the complete expression `foo()` for the overloaded call,
        // which will yield the &T type
        let temp_lifetime = self.region_scope_tree.temporary_scope(expr.hir_id.local_id);
        let fun = self.arena.alloc(self.method_callee(expr, span, overloaded_callee));
        let ref_expr = self.arena.alloc(Expr {
            temp_lifetime,
            ty: ref_ty,
            span,
            kind: ExprKind::Call { ty: fun.ty, fun, args, from_hir_call: false, fn_span: span },
        });

        // construct and return a deref wrapper `*foo()`
        ExprKind::Deref { arg: ref_expr }
    }

    fn convert_captured_hir_place(
        &mut self,
        closure_expr: &'tcx hir::Expr<'tcx>,
        place: HirPlace<'tcx>,
    ) -> Expr<'thir, 'tcx> {
        let temp_lifetime = self.region_scope_tree.temporary_scope(closure_expr.hir_id.local_id);
        let var_ty = place.base_ty;

        // The result of capture analysis in `rustc_typeck/check/upvar.rs`represents a captured path
        // as it's seen for use within the closure and not at the time of closure creation.
        //
        // That is we see expect to see it start from a captured upvar and not something that is local
        // to the closure's parent.
        let var_hir_id = match place.base {
            HirPlaceBase::Upvar(upvar_id) => upvar_id.var_path.hir_id,
            base => bug!("Expected an upvar, found {:?}", base),
        };

        let mut captured_place_expr = Expr {
            temp_lifetime,
            ty: var_ty,
            span: closure_expr.span,
            kind: self.convert_var(var_hir_id),
        };

        for proj in place.projections.iter() {
            let kind = match proj.kind {
                HirProjectionKind::Deref => {
                    ExprKind::Deref { arg: self.arena.alloc(captured_place_expr) }
                }
                HirProjectionKind::Field(field, ..) => {
                    // Variant index will always be 0, because for multi-variant
                    // enums, we capture the enum entirely.
                    ExprKind::Field {
                        lhs: self.arena.alloc(captured_place_expr),
                        name: Field::new(field as usize),
                    }
                }
                HirProjectionKind::Index | HirProjectionKind::Subslice => {
                    // We don't capture these projections, so we can ignore them here
                    continue;
                }
            };

            captured_place_expr =
                Expr { temp_lifetime, ty: proj.ty, span: closure_expr.span, kind };
        }

        captured_place_expr
    }

    fn capture_upvar(
        &mut self,
        closure_expr: &'tcx hir::Expr<'tcx>,
        captured_place: &'tcx ty::CapturedPlace<'tcx>,
        upvar_ty: Ty<'tcx>,
    ) -> Expr<'thir, 'tcx> {
        let upvar_capture = captured_place.info.capture_kind;
        let captured_place_expr =
            self.convert_captured_hir_place(closure_expr, captured_place.place.clone());
        let temp_lifetime = self.region_scope_tree.temporary_scope(closure_expr.hir_id.local_id);

        match upvar_capture {
            ty::UpvarCapture::ByValue(_) => captured_place_expr,
            ty::UpvarCapture::ByRef(upvar_borrow) => {
                let borrow_kind = match upvar_borrow.kind {
                    ty::BorrowKind::ImmBorrow => BorrowKind::Shared,
                    ty::BorrowKind::UniqueImmBorrow => BorrowKind::Unique,
                    ty::BorrowKind::MutBorrow => BorrowKind::Mut { allow_two_phase_borrow: false },
                };
                Expr {
                    temp_lifetime,
                    ty: upvar_ty,
                    span: closure_expr.span,
                    kind: ExprKind::Borrow {
                        borrow_kind,
                        arg: self.arena.alloc(captured_place_expr),
                    },
                }
            }
        }
    }

    /// Converts a list of named fields (i.e., for struct-like struct/enum ADTs) into FieldExpr.
    fn field_refs(
        &mut self,
        fields: &'tcx [hir::ExprField<'tcx>],
    ) -> &'thir [FieldExpr<'thir, 'tcx>] {
        self.arena.alloc_from_iter(fields.iter().map(|field| FieldExpr {
            name: Field::new(self.tcx.field_index(field.hir_id, self.typeck_results)),
            expr: self.mirror_expr(field.expr),
        }))
    }
}

trait ToBorrowKind {
    fn to_borrow_kind(&self) -> BorrowKind;
}

impl ToBorrowKind for AutoBorrowMutability {
    fn to_borrow_kind(&self) -> BorrowKind {
        use rustc_middle::ty::adjustment::AllowTwoPhase;
        match *self {
            AutoBorrowMutability::Mut { allow_two_phase_borrow } => BorrowKind::Mut {
                allow_two_phase_borrow: match allow_two_phase_borrow {
                    AllowTwoPhase::Yes => true,
                    AllowTwoPhase::No => false,
                },
            },
            AutoBorrowMutability::Not => BorrowKind::Shared,
        }
    }
}

impl ToBorrowKind for hir::Mutability {
    fn to_borrow_kind(&self) -> BorrowKind {
        match *self {
            hir::Mutability::Mut => BorrowKind::Mut { allow_two_phase_borrow: false },
            hir::Mutability::Not => BorrowKind::Shared,
        }
    }
}

fn bin_op(op: hir::BinOpKind) -> BinOp {
    match op {
        hir::BinOpKind::Add => BinOp::Add,
        hir::BinOpKind::Sub => BinOp::Sub,
        hir::BinOpKind::Mul => BinOp::Mul,
        hir::BinOpKind::Div => BinOp::Div,
        hir::BinOpKind::Rem => BinOp::Rem,
        hir::BinOpKind::BitXor => BinOp::BitXor,
        hir::BinOpKind::BitAnd => BinOp::BitAnd,
        hir::BinOpKind::BitOr => BinOp::BitOr,
        hir::BinOpKind::Shl => BinOp::Shl,
        hir::BinOpKind::Shr => BinOp::Shr,
        hir::BinOpKind::Eq => BinOp::Eq,
        hir::BinOpKind::Lt => BinOp::Lt,
        hir::BinOpKind::Le => BinOp::Le,
        hir::BinOpKind::Ne => BinOp::Ne,
        hir::BinOpKind::Ge => BinOp::Ge,
        hir::BinOpKind::Gt => BinOp::Gt,
        _ => bug!("no equivalent for ast binop {:?}", op),
    }
}
