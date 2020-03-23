//! Serialised Intermideiate Representation (SIR).
//!
//! SIR is built in-memory during LLVM code-generation, and finally placed into an ELF section at
//! link time.

#![allow(dead_code, unused_imports)]

use crate::llvm::{self, BasicBlock};
use crate::value::Value;
use crate::{common, ModuleLlvm};
use rustc::ty::TyCtxt;
use rustc_codegen_ssa::{ModuleCodegen, ModuleKind};
use rustc_data_structures::fx::FxHashMap;
use rustc_data_structures::small_c_str::SmallCStr;
use rustc_hir::def_id::LOCAL_CRATE;
use rustc_index::{
    newtype_index,
    vec::{Idx, IndexVec},
};
use rustc_session::config::OutputType;
use std::default::Default;
use std::ffi::CString;
use ykpack;

const SIR_SECTION: &str = ".yk_sir";
const SIR_GLOBAL_SYM_PREFIX: &str = ".yksir";

/// Writes the SIR into a buffer which will be linked in into an ELF section via LLVM.
/// This is based on write_compressed_metadata().
pub fn write_sir<'tcx>(tcx: TyCtxt<'tcx>, sir_llvm_module: &mut ModuleLlvm) {
    let sir_funcs = tcx.sir.funcs.replace(Vec::new());
    let mut buf = Vec::new();
    let mut encoder = ykpack::Encoder::from(&mut buf);

    for func in sir_funcs {
        encoder.serialise(ykpack::Pack::Body(func)).unwrap();
    }

    encoder.done().unwrap();

    let (sir_llcx, sir_llmod) = (&*sir_llvm_module.llcx, sir_llvm_module.llmod());
    let llmeta = common::bytes_in_context(sir_llcx, &buf);
    let llconst = common::struct_in_context(sir_llcx, &[llmeta], false);

    // Borrowed from exported_symbols::metadata_symbol_name().
    let sym_name = format!(
        "{}_{}_{}",
        SIR_GLOBAL_SYM_PREFIX,
        tcx.original_crate_name(LOCAL_CRATE),
        tcx.crate_disambiguator(LOCAL_CRATE).to_fingerprint().to_hex()
    );

    let buf = CString::new(sym_name).unwrap();
    let llglobal = unsafe { llvm::LLVMAddGlobal(sir_llmod, common::val_ty(llconst), buf.as_ptr()) };

    let section_name =
        format!("{}{}", ykpack::SIR_SECTION_PREFIX, &*tcx.crate_name(LOCAL_CRATE).as_str());
    unsafe {
        llvm::LLVMSetInitializer(llglobal, llconst);
        let name = SmallCStr::new(&section_name);
        llvm::LLVMSetSection(llglobal, name.as_ptr());

        // Following the precedent of write_compressed_metadata(), force empty flags so that
        // the SIR doesn't get loaded into memory.
        let directive = format!(".section {}, \"\", @progbits", &section_name);
        let directive = CString::new(directive).unwrap();
        llvm::LLVMSetModuleInlineAsm(sir_llmod, directive.as_ptr())
    }
}
