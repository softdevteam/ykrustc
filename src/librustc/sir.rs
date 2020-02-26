//! Serialised Intermediate Representation (SIR).
//!
//! SIR is built in-memory during LLVM code-generation, and finally placed into a dedicated ELF
//! section at link time.

use rustc_data_structures::fx::FxHashMap;
use rustc_index::{newtype_index, vec::IndexVec};
use std::collections::hash_map::Entry;
use std::default::Default;
use std::io::{self, Write};
use ykpack;

// Duplicates of LLVM types defined elsewhere, copied to avoid cyclic dependencies. Whereas the
// LLVM backend expresses pointers to these using references, we use raw pointers so to as avoid
// introducing lifetime parameters to the SirCx (and thus into TyCtxt and every place that uses
// it).
extern "C" {
    pub type Value;
}
extern "C" {
    pub type BasicBlock;
}
extern "C" {
    pub type Builder;
}

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

// A local variable index.
// Note that these indices are not globally unique. For a globally unique local variable
// identifier, a (SirFuncIdx, SirLocalIdx) pair must be used.
newtype_index! {
    pub struct SirLocalIdx {
        DEBUG_FORMAT = "SirLocalIdx({})"
    }
}

impl Into<ykpack::Local> for SirLocalIdx {
    fn into(self) -> ykpack::Local {
        ykpack::Local(self.as_u32())
    }
}

/// Sir equivalents of LLVM values.
#[derive(Debug)]
pub enum SirValue {
    Func(SirFuncIdx),
    /// A variable local to a function.
    Local(SirFuncIdx, SirLocalIdx),
}

impl SirValue {
    pub fn func(&self) -> SirFuncIdx {
        if let Self::Func(idx) = self {
            *idx
        } else {
            panic!("tried to make a function from something else");
        }
    }

    pub fn local(&self) -> (SirFuncIdx, SirLocalIdx) {
        if let Self::Local(func_idx, local_idx) = self {
            (*func_idx, *local_idx)
        } else {
            panic!("tried to make a local from something else");
        }
    }
}

pub struct SirCx {
    /// Maps an opaque LLVM `Value` to its SIR equivalent.
    pub llvm_values: FxHashMap<*const Value, SirValue>,
    /// Maps an opaque LLVM `BasicBlock` to the function and block index of its SIR equivalent.
    pub llvm_blocks: FxHashMap<*const BasicBlock, (SirFuncIdx, SirBlockIdx)>,
    /// Function store. Also owns the blocks
    pub funcs: IndexVec<SirFuncIdx, ykpack::Body>,
    /// Keeps track of local variables on a per-function basis.
    pub locals: IndexVec<SirFuncIdx, IndexVec<SirLocalIdx, *const Value>>,
    /// Mirrors the insertion point for each LLVM `IrBuilder`.
    pub builders: FxHashMap<*const Builder, (*const BasicBlock, usize)>,
}

impl SirCx {
    pub fn new() -> Self {
        Self {
            llvm_values: Default::default(),
            llvm_blocks: FxHashMap::default(),
            funcs: Default::default(),
            locals: Default::default(),
            builders: Default::default(),
        }
    }

    /// Gets the Sir local variable index for the LLVM pointer `local`. If we don't know about the
    /// pointer yet, this function creates a new index, updates our records and returns the new
    /// index.
    fn get_or_add_local(
        &mut self,
        func_idx: SirFuncIdx,
        local: *const Value,
    ) -> (SirFuncIdx, SirLocalIdx) {
        match self.llvm_values.entry(local) {
            Entry::Occupied(e) => {
                let (got_func_idx, got_local_idx) = e.get().local();
                debug_assert!(got_func_idx == func_idx);
                (got_func_idx, got_local_idx)
            }
            Entry::Vacant(e) => {
                let local_idx = SirLocalIdx::from_usize(self.locals[func_idx].len());
                self.locals[func_idx].push(local);
                e.insert(SirValue::Local(func_idx, local_idx));
                (func_idx, local_idx)
            }
        }
    }

    pub fn add_func(&mut self, value: *const Value, symbol_name: String) {
        let idx = SirFuncIdx::from_usize(self.funcs.len());

        self.funcs.push(ykpack::Body {
            symbol_name,
            blocks: Default::default(),
            flags: 0, // Set later.
        });
        let existing = self.llvm_values.insert(value, SirValue::Func(idx));
        // In theory, if a function is declared twice, then LLVM should return the same pointer
        // each time (i.e. it updates the existing record). This doesn't seem to happen though, as
        // proven by this assertion.
        debug_assert!(existing.is_none());

        // Make space for the function's local variables.
        self.locals.push(Default::default());
    }

    pub fn add_block(&mut self, func: *const Value, block: *const BasicBlock) {
        let func_idx = self.llvm_values[&func].func();
        let sir_func = &mut self.funcs[func_idx];
        let block_idx = SirBlockIdx::from_usize(sir_func.blocks.len());
        sir_func.blocks.push(ykpack::BasicBlock {
            stmts: Default::default(),
            term: ykpack::Terminator::Unreachable, // FIXME
        });
        let existing = self.llvm_blocks.insert(block, (func_idx, block_idx));
        debug_assert!(existing.is_none());
    }

    pub fn get_symbol_name(&mut self, func: *const Value) -> &String {
        let func_idx = self.llvm_values[&func].func();
        let sir_func = &mut self.funcs[func_idx];
        &sir_func.symbol_name
    }

    /// For hardware tracing, during codegen we insert DILabels to know where we are in the binary.
    /// These labels must be emitted in a deterministic order otherwise the reproducible build
    /// checker gets upset. This function gives the codegen what it needs in a data structure which
    /// can be iterated deterministically.
    pub fn funcs_and_blocks_deterministic(
        &self,
    ) -> IndexVec<SirFuncIdx, IndexVec<SirBlockIdx, *const BasicBlock>> {
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

    /// Given an llvm::BasicBlock returns the equivalent ykpack::BasicBlock in the SIR.
    fn get_sir_block(&mut self, bb: *const BasicBlock) -> &mut ykpack::BasicBlock {
        let (sirfuncidx, sirblockidx) = self.llvm_blocks[&bb];
        let sir_func = &mut self.funcs[sirfuncidx];
        &mut sir_func.blocks[sirblockidx.index()]
    }

    /// Set the current position of builder to `pos`. Equivalent to LLVMPositionBuilderBefore.
    pub fn position_before(&mut self, builder: *const Builder, bb: *const BasicBlock, pos: usize) {
        self.builders.insert(builder, (bb, pos));
    }

    /// Set the current position of builder to the end of `bb`. Equivalent to
    /// LLVMPositionBuilderAtEnd.
    pub fn position_at_end(&mut self, builder: *const Builder, bb: *const BasicBlock) {
        let sir_block = self.get_sir_block(bb);
        let pos = sir_block.stmts.len();
        self.builders.insert(builder, (bb, pos));
    }

    /// Inserts a statement at the insertion point corresponding with `builder`.
    pub fn emit(&mut self, builder: *const Builder, stmt: ykpack::Statement) {
        let (bb, idx) = self.builders[&builder];
        let sir_block = self.get_sir_block(bb);
        sir_block.stmts.insert(idx, stmt);
    }

    /// Returns the function containing the insertion point of the supplied builder.
    pub fn current_func(&mut self, builder: *const Builder) -> SirFuncIdx {
        let (llbb, _) = self.builders[&builder];
        self.llvm_blocks[&llbb].0
    }

    pub fn emit_assign(
        &mut self,
        builder: *const Builder,
        result: SirLocalIdx,
        rhs: ykpack::Rvalue,
    ) {
        self.emit(builder, ykpack::Statement::Assign(result.into(), rhs));
    }

    pub fn emit_load(&mut self, builder: *const Builder, result: *const Value, arg: *const Value) {
        // FIXME deal with volatile loads.
        let func_idx = self.current_func(builder);
        let arg_local = self.get_or_add_local(func_idx, arg).1;
        // FIXME this assumes that the argument is a local for now.
        let res_local = self.get_or_add_local(func_idx, result).1;
        self.emit(
            builder,
            ykpack::Statement::Assign(res_local.into(), ykpack::Rvalue::Load(arg_local.as_u32())),
        );
    }

    pub fn emit_store(&mut self, builder: *const Builder) {
        // FIXME argument to store.
        self.emit(builder, ykpack::Statement::Store);
    }

    pub fn emit_ret(&mut self, builder: *const Builder) {
        // FIXME optional argument to return terminator.
        self.emit(builder, ykpack::Statement::Return);
    }
}
