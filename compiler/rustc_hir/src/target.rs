//! This module implements some validity checks for attributes.
//! In particular it verifies that `#[inline]` and `#[repr]` attributes are
//! attached to items that actually support them and if there are
//! conflicts between multiple such attributes attached to the same
//! item.

use crate::hir;
use crate::{Item, ItemKind, TraitItem, TraitItemKind};

use std::fmt::{self, Display};

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum GenericParamKind {
    Type,
    Lifetime,
    Const,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum MethodKind {
    Trait { body: bool },
    Inherent,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Target {
    ExternCrate,
    Use,
    Static,
    Const,
    Fn,
    Closure,
    Mod,
    ForeignMod,
    GlobalAsm,
    TyAlias,
    OpaqueTy,
    Enum,
    Variant,
    Struct,
    Field,
    Union,
    Trait,
    TraitAlias,
    Impl,
    Expression,
    Statement,
    Arm,
    AssocConst,
    Method(MethodKind),
    AssocTy,
    ForeignFn,
    ForeignStatic,
    ForeignTy,
    GenericParam(GenericParamKind),
    MacroDef,
    Param,
}

impl Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match *self {
                Target::ExternCrate => "extern crate",
                Target::Use => "use",
                Target::Static => "static item",
                Target::Const => "constant item",
                Target::Fn => "function",
                Target::Closure => "closure",
                Target::Mod => "module",
                Target::ForeignMod => "foreign module",
                Target::GlobalAsm => "global asm",
                Target::TyAlias => "type alias",
                Target::OpaqueTy => "opaque type",
                Target::Enum => "enum",
                Target::Variant => "enum variant",
                Target::Struct => "struct",
                Target::Field => "struct field",
                Target::Union => "union",
                Target::Trait => "trait",
                Target::TraitAlias => "trait alias",
                Target::Impl => "item",
                Target::Expression => "expression",
                Target::Statement => "statement",
                Target::Arm => "match arm",
                Target::AssocConst => "associated const",
                Target::Method(_) => "method",
                Target::AssocTy => "associated type",
                Target::ForeignFn => "foreign function",
                Target::ForeignStatic => "foreign static item",
                Target::ForeignTy => "foreign type",
                Target::GenericParam(kind) => match kind {
                    GenericParamKind::Type => "type parameter",
                    GenericParamKind::Lifetime => "lifetime parameter",
                    GenericParamKind::Const => "const parameter",
                },
                Target::MacroDef => "macro def",
                Target::Param => "function param",
            }
        )
    }
}

impl Target {
    pub fn from_item(item: &Item<'_>) -> Target {
        match item.kind {
            ItemKind::ExternCrate(..) => Target::ExternCrate,
            ItemKind::Use(..) => Target::Use,
            ItemKind::Static(..) => Target::Static,
            ItemKind::Const(..) => Target::Const,
            ItemKind::Fn(..) => Target::Fn,
            ItemKind::Mod(..) => Target::Mod,
            ItemKind::ForeignMod { .. } => Target::ForeignMod,
            ItemKind::GlobalAsm(..) => Target::GlobalAsm,
            ItemKind::TyAlias(..) => Target::TyAlias,
            ItemKind::OpaqueTy(..) => Target::OpaqueTy,
            ItemKind::Enum(..) => Target::Enum,
            ItemKind::Struct(..) => Target::Struct,
            ItemKind::Union(..) => Target::Union,
            ItemKind::Trait(..) => Target::Trait,
            ItemKind::TraitAlias(..) => Target::TraitAlias,
            ItemKind::Impl { .. } => Target::Impl,
        }
    }

    pub fn from_trait_item(trait_item: &TraitItem<'_>) -> Target {
        match trait_item.kind {
            TraitItemKind::Const(..) => Target::AssocConst,
            TraitItemKind::Fn(_, hir::TraitFn::Required(_)) => {
                Target::Method(MethodKind::Trait { body: false })
            }
            TraitItemKind::Fn(_, hir::TraitFn::Provided(_)) => {
                Target::Method(MethodKind::Trait { body: true })
            }
            TraitItemKind::Type(..) => Target::AssocTy,
        }
    }

    pub fn from_foreign_item(foreign_item: &hir::ForeignItem<'_>) -> Target {
        match foreign_item.kind {
            hir::ForeignItemKind::Fn(..) => Target::ForeignFn,
            hir::ForeignItemKind::Static(..) => Target::ForeignStatic,
            hir::ForeignItemKind::Type => Target::ForeignTy,
        }
    }

    pub fn from_generic_param(generic_param: &hir::GenericParam<'_>) -> Target {
        match generic_param.kind {
            hir::GenericParamKind::Type { .. } => Target::GenericParam(GenericParamKind::Type),
            hir::GenericParamKind::Lifetime { .. } => {
                Target::GenericParam(GenericParamKind::Lifetime)
            }
            hir::GenericParamKind::Const { .. } => Target::GenericParam(GenericParamKind::Const),
        }
    }
}
