#![feature(rustc_private, decl_macro, never_type, hash_drain_filter)]
#![warn(rust_2018_idioms)]
#![warn(unused_lifetimes)]
#![warn(unreachable_pub)]

extern crate snap;
#[macro_use]
extern crate rustc_middle;
extern crate rustc_ast;
extern crate rustc_codegen_ssa;
extern crate rustc_data_structures;
extern crate rustc_errors;
extern crate rustc_fs_util;
extern crate rustc_hir;
extern crate rustc_incremental;
extern crate rustc_index;
extern crate rustc_session;
extern crate rustc_span;
extern crate rustc_target;

// This prevents duplicating functions and statics that are already part of the host rustc process.
#[allow(unused_extern_crates)]
extern crate rustc_driver;

use std::any::Any;
use std::str::FromStr;

use rustc_codegen_ssa::traits::CodegenBackend;
use rustc_codegen_ssa::CodegenResults;
use rustc_errors::ErrorReported;
use rustc_middle::dep_graph::{WorkProduct, WorkProductId};
use rustc_middle::middle::cstore::{EncodedMetadata, MetadataLoader};
use rustc_middle::ty::query::Providers;
use rustc_session::config::OutputFilenames;
use rustc_session::Session;

use cranelift_codegen::settings::{self, Configurable};

use crate::constant::ConstantCx;
use crate::prelude::*;

mod abi;
mod allocator;
mod analyze;
mod archive;
mod backend;
mod base;
mod cast;
mod codegen_i128;
mod common;
mod compiler_builtins;
mod constant;
mod debuginfo;
mod discriminant;
mod driver;
mod inline_asm;
mod intrinsics;
mod linkage;
mod main_shim;
mod metadata;
mod num;
mod optimize;
mod pointer;
mod pretty_clif;
mod toolchain;
mod trap;
mod unsize;
mod value_and_place;
mod vtable;

mod prelude {
    pub(crate) use std::convert::{TryFrom, TryInto};

    pub(crate) use rustc_span::Span;

    pub(crate) use rustc_hir::def_id::{DefId, LOCAL_CRATE};
    pub(crate) use rustc_middle::bug;
    pub(crate) use rustc_middle::mir::{self, *};
    pub(crate) use rustc_middle::ty::layout::{self, TyAndLayout};
    pub(crate) use rustc_middle::ty::{
        self, FloatTy, Instance, InstanceDef, IntTy, ParamEnv, Ty, TyCtxt, TypeAndMut,
        TypeFoldable, UintTy,
    };
    pub(crate) use rustc_target::abi::{Abi, LayoutOf, Scalar, Size, VariantIdx};

    pub(crate) use rustc_data_structures::fx::FxHashMap;

    pub(crate) use rustc_index::vec::Idx;

    pub(crate) use cranelift_codegen::entity::EntitySet;
    pub(crate) use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
    pub(crate) use cranelift_codegen::ir::function::Function;
    pub(crate) use cranelift_codegen::ir::types;
    pub(crate) use cranelift_codegen::ir::{
        AbiParam, Block, ExternalName, FuncRef, Inst, InstBuilder, MemFlags, Signature, SourceLoc,
        StackSlot, StackSlotData, StackSlotKind, TrapCode, Type, Value,
    };
    pub(crate) use cranelift_codegen::isa::{self, CallConv};
    pub(crate) use cranelift_codegen::Context;
    pub(crate) use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
    pub(crate) use cranelift_module::{self, DataContext, DataId, FuncId, Linkage, Module};

    pub(crate) use crate::abi::*;
    pub(crate) use crate::base::{codegen_operand, codegen_place};
    pub(crate) use crate::cast::*;
    pub(crate) use crate::common::*;
    pub(crate) use crate::debuginfo::{DebugContext, UnwindContext};
    pub(crate) use crate::pointer::Pointer;
    pub(crate) use crate::trap::*;
    pub(crate) use crate::value_and_place::{CPlace, CPlaceInner, CValue};
}

struct PrintOnPanic<F: Fn() -> String>(F);
impl<F: Fn() -> String> Drop for PrintOnPanic<F> {
    fn drop(&mut self) {
        if ::std::thread::panicking() {
            println!("{}", (self.0)());
        }
    }
}

struct CodegenCx<'m, 'tcx: 'm> {
    tcx: TyCtxt<'tcx>,
    module: &'m mut dyn Module,
    global_asm: String,
    constants_cx: ConstantCx,
    cached_context: Context,
    vtables: FxHashMap<(Ty<'tcx>, Option<ty::PolyExistentialTraitRef<'tcx>>), DataId>,
    debug_context: Option<DebugContext<'tcx>>,
    unwind_context: UnwindContext<'tcx>,
}

impl<'m, 'tcx> CodegenCx<'m, 'tcx> {
    fn new(
        tcx: TyCtxt<'tcx>,
        backend_config: BackendConfig,
        module: &'m mut dyn Module,
        debug_info: bool,
    ) -> Self {
        let unwind_context = UnwindContext::new(
            tcx,
            module.isa(),
            matches!(backend_config.codegen_mode, CodegenMode::Aot),
        );
        let debug_context =
            if debug_info { Some(DebugContext::new(tcx, module.isa())) } else { None };
        CodegenCx {
            tcx,
            module,
            global_asm: String::new(),
            constants_cx: ConstantCx::default(),
            cached_context: Context::new(),
            vtables: FxHashMap::default(),
            debug_context,
            unwind_context,
        }
    }

    fn finalize(self) -> (String, Option<DebugContext<'tcx>>, UnwindContext<'tcx>) {
        self.constants_cx.finalize(self.tcx, self.module);
        (self.global_asm, self.debug_context, self.unwind_context)
    }
}

#[derive(Copy, Clone, Debug)]
pub enum CodegenMode {
    Aot,
    Jit,
    JitLazy,
}

impl Default for CodegenMode {
    fn default() -> Self {
        CodegenMode::Aot
    }
}

impl FromStr for CodegenMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "aot" => Ok(CodegenMode::Aot),
            "jit" => Ok(CodegenMode::Jit),
            "jit-lazy" => Ok(CodegenMode::JitLazy),
            _ => Err(format!("Unknown codegen mode `{}`", s)),
        }
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct BackendConfig {
    pub codegen_mode: CodegenMode,
}

impl BackendConfig {
    fn from_opts(opts: &[String]) -> Result<Self, String> {
        let mut config = BackendConfig::default();
        for opt in opts {
            if let Some((name, value)) = opt.split_once('=') {
                match name {
                    "mode" => config.codegen_mode = value.parse()?,
                    _ => return Err(format!("Unknown option `{}`", name)),
                }
            } else {
                return Err(format!("Invalid option `{}`", opt));
            }
        }
        Ok(config)
    }
}

pub struct CraneliftCodegenBackend {
    pub config: Option<BackendConfig>,
}

impl CodegenBackend for CraneliftCodegenBackend {
    fn init(&self, sess: &Session) {
        use rustc_session::config::Lto;
        match sess.lto() {
            Lto::No | Lto::ThinLocal => {}
            Lto::Thin | Lto::Fat => sess.warn("LTO is not supported. You may get a linker error."),
        }
    }

    fn metadata_loader(&self) -> Box<dyn MetadataLoader + Sync> {
        Box::new(crate::metadata::CraneliftMetadataLoader)
    }

    fn provide(&self, _providers: &mut Providers) {}
    fn provide_extern(&self, _providers: &mut Providers) {}

    fn target_features(&self, _sess: &Session) -> Vec<rustc_span::Symbol> {
        vec![]
    }

    fn codegen_crate(
        &self,
        tcx: TyCtxt<'_>,
        metadata: EncodedMetadata,
        need_metadata_module: bool,
    ) -> Box<dyn Any> {
        let config = if let Some(config) = self.config {
            config
        } else {
            BackendConfig::from_opts(&tcx.sess.opts.cg.llvm_args)
                .unwrap_or_else(|err| tcx.sess.fatal(&err))
        };
        driver::codegen_crate(tcx, metadata, need_metadata_module, config)
    }

    fn join_codegen(
        &self,
        ongoing_codegen: Box<dyn Any>,
        _sess: &Session,
    ) -> Result<(CodegenResults, FxHashMap<WorkProductId, WorkProduct>), ErrorReported> {
        Ok(*ongoing_codegen
            .downcast::<(CodegenResults, FxHashMap<WorkProductId, WorkProduct>)>()
            .unwrap())
    }

    fn link(
        &self,
        sess: &Session,
        codegen_results: CodegenResults,
        outputs: &OutputFilenames,
    ) -> Result<(), ErrorReported> {
        use rustc_codegen_ssa::back::link::link_binary;

        let target_cpu = crate::target_triple(sess).to_string();
        link_binary::<crate::archive::ArArchiveBuilder<'_>>(
            sess,
            &codegen_results,
            outputs,
            &codegen_results.crate_name.as_str(),
            &target_cpu,
        );

        Ok(())
    }
}

fn target_triple(sess: &Session) -> target_lexicon::Triple {
    sess.target.llvm_target.parse().unwrap()
}

fn build_isa(sess: &Session) -> Box<dyn isa::TargetIsa + 'static> {
    use target_lexicon::BinaryFormat;

    let target_triple = crate::target_triple(sess);

    let mut flags_builder = settings::builder();
    flags_builder.enable("is_pic").unwrap();
    flags_builder.set("enable_probestack", "false").unwrap(); // __cranelift_probestack is not provided
    let enable_verifier =
        cfg!(debug_assertions) || std::env::var("CG_CLIF_ENABLE_VERIFIER").is_ok();
    flags_builder.set("enable_verifier", if enable_verifier { "true" } else { "false" }).unwrap();

    let tls_model = match target_triple.binary_format {
        BinaryFormat::Elf => "elf_gd",
        BinaryFormat::Macho => "macho",
        BinaryFormat::Coff => "coff",
        _ => "none",
    };
    flags_builder.set("tls_model", tls_model).unwrap();

    flags_builder.set("enable_simd", "true").unwrap();

    flags_builder.set("enable_llvm_abi_extensions", "true").unwrap();

    use rustc_session::config::OptLevel;
    match sess.opts.optimize {
        OptLevel::No => {
            flags_builder.set("opt_level", "none").unwrap();
        }
        OptLevel::Less | OptLevel::Default => {}
        OptLevel::Size | OptLevel::SizeMin | OptLevel::Aggressive => {
            flags_builder.set("opt_level", "speed_and_size").unwrap();
        }
    }

    let flags = settings::Flags::new(flags_builder);

    let variant = cranelift_codegen::isa::BackendVariant::MachInst;
    let mut isa_builder = cranelift_codegen::isa::lookup_variant(target_triple, variant).unwrap();
    // Don't use "haswell", as it implies `has_lzcnt`.macOS CI is still at Ivy Bridge EP, so `lzcnt`
    // is interpreted as `bsr`.
    isa_builder.enable("nehalem").unwrap();
    isa_builder.finish(flags)
}

/// This is the entrypoint for a hot plugged rustc_codegen_cranelift
#[no_mangle]
pub fn __rustc_codegen_backend() -> Box<dyn CodegenBackend> {
    Box::new(CraneliftCodegenBackend { config: None })
}
