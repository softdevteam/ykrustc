//! Serialised Intermediate Representation (SIR).
//!
//! SIR is built in-memory during LLVM code-generation, and finally placed into an ELF section at
//! link time.

#![allow(dead_code, unused_imports)]

use crate::llvm::{self, BasicBlock};
use crate::value::Value;
use crate::{common, context::CodegenCx, ModuleLlvm};
use rustc_codegen_ssa::{traits::SirMethods, ModuleCodegen, ModuleKind};
use rustc_data_structures::fx::FxHashMap;
use rustc_data_structures::small_c_str::SmallCStr;
use rustc_hir::def_id::LOCAL_CRATE;
use rustc_index::{
    newtype_index,
    vec::{Idx, IndexVec},
};
use rustc_middle::ty::TyCtxt;
use rustc_session::config::OutputType;
use std::convert::TryFrom;
use std::default::Default;
use std::ffi::CString;
use ykpack;

const SIR_SECTION: &str = ".yk_sir";
const SIR_GLOBAL_SYM_PREFIX: &str = ".yksir";

/// Writes the SIR into a buffer which will be linked in into an ELF section via LLVM.
/// This is based on write_compressed_metadata().
pub fn write_sir<'tcx>(
    tcx: TyCtxt<'tcx>,
    llvm_module: &ModuleLlvm,
    cgu_name: &str,
    sir_types: rustc_codegen_ssa::sir::SirTypes,
    sir_funcs: Vec<ykpack::Body>,
) {
    let mut data_buf = Vec::new();
    let mut encoder = ykpack::Encoder::from(&mut data_buf);
    let mut hdr = ykpack::SirHeader::new(sir_types.cgu_hash);

    // First we serialise the types which will be referenced in the body packs that will follow.
    // The serialisation order matters here, as the load order (in the runtime) corresponds with
    // the type indices, hence use of `IndexMap` for insertion order.
    for (typ, typ_idx) in sir_types.map {
        debug_assert!(usize::try_from(typ_idx).unwrap() == hdr.types.len());
        hdr.types.push(encoder.tell());
        encoder.serialise(ykpack::Pack::Type(typ)).unwrap();
    }

    for func in sir_funcs {
        hdr.bodies.insert(func.symbol_name.clone(), encoder.tell());
        encoder.serialise(ykpack::Pack::Body(func)).unwrap();
    }

    // Now we encode the header and prepend it to what we encoded above.
    // All offsets are therefore relative to the end of the header.
    let mut buf = Vec::new();
    let mut hdr_encoder = ykpack::Encoder::from(&mut buf);
    hdr_encoder.serialise(ykpack::Pack::Header(hdr)).unwrap();
    buf.append(&mut data_buf);

    let (sir_llcx, sir_llmod) = (&*llvm_module.llcx, llvm_module.llmod());
    let llmeta = common::bytes_in_context(sir_llcx, &buf);
    let llconst = common::struct_in_context(sir_llcx, &[llmeta], false);

    // Borrowed from exported_symbols::metadata_symbol_name().
    let sym_name = format!(
        "{}_{}_{}_{}_sym",
        SIR_GLOBAL_SYM_PREFIX,
        tcx.original_crate_name(LOCAL_CRATE),
        tcx.crate_disambiguator(LOCAL_CRATE).to_fingerprint().to_hex(),
        cgu_name,
    );

    let buf = CString::new(sym_name.clone()).unwrap();
    let llglobal = unsafe { llvm::LLVMAddGlobal(sir_llmod, common::val_ty(llconst), buf.as_ptr()) };

    let section_name = format!(
        "{}_{}_{}_{}",
        SIR_GLOBAL_SYM_PREFIX,
        tcx.original_crate_name(LOCAL_CRATE),
        tcx.crate_disambiguator(LOCAL_CRATE).to_fingerprint().to_hex(),
        cgu_name,
    );
    unsafe {
        llvm::LLVMSetInitializer(llglobal, llconst);
        let name = SmallCStr::new(&section_name);
        llvm::LLVMSetSection(llglobal, name.as_ptr());

        // Following the precedent of write_compressed_metadata(), force empty flags so that
        // the SIR doesn't get loaded into memory.
        let directive = format!(".section {}, \"\", @progbits", &section_name);
        llvm::LLVMRustAppendModuleInlineAsm(sir_llmod, directive.as_ptr().cast(), directive.len());
    }
}

impl SirMethods for CodegenCx<'b, 'tcx> {
    fn define_sir_type(&self, ty: ykpack::Ty) -> ykpack::TypeId {
        let mut types = self.sir.as_ref().unwrap().types.borrow_mut();
        (types.cgu_hash, types.index(ty))
    }

    fn define_function_sir(&self, sir: ykpack::Body) {
        self.sir.as_ref().unwrap().funcs.borrow_mut().push(sir);
    }

    fn get_size_align(&self, tyid: ykpack::TypeId) -> (usize, usize) {
        let types = self.sir.as_ref().unwrap().types.borrow();
        let ty = types.get(tyid);
        (ty.size, ty.align)
    }
}
