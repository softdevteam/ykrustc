//! Serialised Intermediate Representation (SIR).
//!
//! SIR is built in-memory during LLVM code-generation, and finally placed into a dedicated ELF
//! section at link time.

use std::default::Default;
use std::io::{self, Write};
use rustc_index::{newtype_index, vec::{Idx, IndexVec}};
use rustc_data_structures::fx::FxHashMap;
use ykpack;

// Duplicates of LLVM types defined elsewhere, copied to avoid cyclic dependencies. Whereas the
// LLVM backend expresses pointers to these using references, we use raw pointers so to as avoid
// introducing lifetime parameters to the SirCx (and thus into TyCtxt and every place that uses
// it).
extern { pub type Value; }
extern { pub type BasicBlock; }

newtype_index! {
    pub struct SirFuncIdx {
        DEBUG_FORMAT = "SirFuncIdx({})"
    }
}

// The index of a block within a function.
// Note that these indices are not globally unique. For a globally unique block identifier, a
// (SirFuncIdx, SirBlockIdx) pair must be used.
newtype_index! {
    pub struct SirBlockIdx {
        DEBUG_FORMAT = "SirBlockIdx({})"
    }
}

/// Sir equivalents of LLVM values.
#[derive(Debug)]
pub enum SirValue {
    Func(SirFuncIdx),
}

impl SirValue {
    pub fn func_idx(&self) -> SirFuncIdx {
        let Self::Func(idx) = self;
        *idx
    }
}

pub struct SirCx {
    /// Maps an opaque LLVM `Value` to its SIR equivalent.
    pub llvm_values: FxHashMap<*const Value, SirValue>,
    /// Maps an opaque LLVM `BasicBlock` to the function and block index of its SIR equivalent.
    pub llvm_blocks: FxHashMap<*const BasicBlock, (SirFuncIdx, SirBlockIdx)>,
    /// Function store. Also owns the blocks
    pub funcs: IndexVec<SirFuncIdx, ykpack::Body>,
}

impl SirCx {
    pub fn new() -> Self {
        Self {
            llvm_values: Default::default(),
            llvm_blocks: FxHashMap::default(),
            funcs: Default::default(),
        }
    }

    pub fn add_func(&mut self, value: *const Value, symbol_name: String) {
        let idx = SirFuncIdx::from_usize(self.funcs.len());

        self.funcs.push(ykpack::Body{
            symbol_name,
            blocks: Default::default(),
            flags: 0,       // Set later.
        });
        let existing = self.llvm_values.insert(value, SirValue::Func(idx));
        // In theory, if a function is declared twice, then LLVM should return the same pointer
        // each time (i.e. it updates the existing record). This doesn't seem to happen though, as
        // proven by this assertion.
        debug_assert!(existing.is_none());
    }

    pub fn add_block(&mut self, func: *const Value, block: *const BasicBlock) {
        let func_idx = self.llvm_values[&func].func_idx();
        let sir_func = &mut self.funcs[func_idx];
        let block_idx = SirBlockIdx::from_usize(sir_func.blocks.len());
        sir_func.blocks.push(ykpack::BasicBlock{
            stmts: Default::default(),
            term: ykpack::Terminator::Unreachable, // FIXME
        });
        let existing = self.llvm_blocks.insert(block, (func_idx, block_idx));
        debug_assert!(existing.is_none());
    }

    pub fn get_symbol_name(&mut self, func: *const Value) -> &String {
        let func_idx = self.llvm_values[&func].func_idx();
        let sir_func = &mut self.funcs[func_idx];
        &sir_func.symbol_name
    }

    /// For hardware tracing, during codegen we insert DILabels to know where we are in the binary.
    /// These labels must be emitted in a deterministic order otherwise the reproducible build
    /// checker gets upset. This function gives the codegen what it needs in a data structure which
    /// can be iterated deterministically.
    pub fn funcs_and_blocks_deterministic(&self)
        -> IndexVec<SirFuncIdx, IndexVec<SirBlockIdx, *const BasicBlock>>
    {
        // We start with a data structure where all LLVM block pointers are unknown (None).
        let mut res = IndexVec::from_elem_n(IndexVec::default(), self.funcs.len());
        for (func_idx, func) in self.funcs.iter_enumerated() {
            res[func_idx] = IndexVec::from_elem_n(None, func.blocks.len());
        }

        // Now we iterate over our hash table, replacing the aforementioned `None`s.
        for (bb, (func_idx, bb_idx)) in &self.llvm_blocks {
            debug_assert!(res[*func_idx][*bb_idx] == None);
            res[*func_idx][*bb_idx] = Some(*bb);
        }

        // Now get rid of the Option wrappers around the pointers. The `unwrap()` is guaranteed to
        // succeed, as the above loop mutates every single `None` to a `Some`.
        let mut ret = IndexVec::default();
        for func_blocks in res.into_iter() {
            ret.push(func_blocks.into_iter().map(|b| b.unwrap()).collect());
        }

        ret
    }

    /// Dump SIR to text file.
    /// Used in tests and for debugging.
    pub fn dump(&self, dest: &mut dyn Write) -> Result<(), io::Error> {
        for func in &self.funcs {
            writeln!(dest, "{}", func)?;
        }

        Ok(())
    }
}
