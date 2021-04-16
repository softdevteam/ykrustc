//! The JIT driver uses [`cranelift_simplejit`] to JIT execute programs without writing any object
//! files.

use std::cell::RefCell;
use std::ffi::CString;
use std::os::raw::{c_char, c_int};

use cranelift_codegen::binemit::{NullStackMapSink, NullTrapSink};
use rustc_codegen_ssa::CrateInfo;
use rustc_middle::mir::mono::MonoItem;
use rustc_session::config::EntryFnType;

use cranelift_jit::{JITBuilder, JITModule};

use crate::{prelude::*, BackendConfig};
use crate::{CodegenCx, CodegenMode};

thread_local! {
    pub static BACKEND_CONFIG: RefCell<Option<BackendConfig>> = RefCell::new(None);
    pub static CURRENT_MODULE: RefCell<Option<JITModule>> = RefCell::new(None);
}

pub(super) fn run_jit(tcx: TyCtxt<'_>, backend_config: BackendConfig) -> ! {
    if !tcx.sess.opts.output_types.should_codegen() {
        tcx.sess.fatal("JIT mode doesn't work with `cargo check`.");
    }

    let imported_symbols = load_imported_symbols_for_jit(tcx);

    let mut jit_builder =
        JITBuilder::with_isa(crate::build_isa(tcx.sess), cranelift_module::default_libcall_names());
    jit_builder.hotswap(matches!(backend_config.codegen_mode, CodegenMode::JitLazy));
    crate::compiler_builtins::register_functions_for_jit(&mut jit_builder);
    jit_builder.symbols(imported_symbols);
    let mut jit_module = JITModule::new(jit_builder);
    assert_eq!(pointer_ty(tcx), jit_module.target_config().pointer_type());

    let (_, cgus) = tcx.collect_and_partition_mono_items(LOCAL_CRATE);
    let mono_items = cgus
        .iter()
        .map(|cgu| cgu.items_in_deterministic_order(tcx).into_iter())
        .flatten()
        .collect::<FxHashMap<_, (_, _)>>()
        .into_iter()
        .collect::<Vec<(_, (_, _))>>();

    let mut cx = crate::CodegenCx::new(tcx, backend_config, &mut jit_module, false);

    super::time(tcx, "codegen mono items", || {
        super::predefine_mono_items(&mut cx, &mono_items);
        for (mono_item, _) in mono_items {
            match mono_item {
                MonoItem::Fn(inst) => match backend_config.codegen_mode {
                    CodegenMode::Aot => unreachable!(),
                    CodegenMode::Jit => {
                        cx.tcx.sess.time("codegen fn", || crate::base::codegen_fn(&mut cx, inst));
                    }
                    CodegenMode::JitLazy => codegen_shim(&mut cx, inst),
                },
                MonoItem::Static(def_id) => {
                    crate::constant::codegen_static(&mut cx.constants_cx, def_id);
                }
                MonoItem::GlobalAsm(item_id) => {
                    let item = cx.tcx.hir().item(item_id);
                    tcx.sess.span_fatal(item.span, "Global asm is not supported in JIT mode");
                }
            }
        }
    });

    let (global_asm, _debug, mut unwind_context) =
        tcx.sess.time("finalize CodegenCx", || cx.finalize());
    jit_module.finalize_definitions();

    if !global_asm.is_empty() {
        tcx.sess.fatal("Inline asm is not supported in JIT mode");
    }

    crate::allocator::codegen(tcx, &mut jit_module, &mut unwind_context);

    tcx.sess.abort_if_errors();

    jit_module.finalize_definitions();
    let _unwind_register_guard = unsafe { unwind_context.register_jit(&jit_module) };

    println!(
        "Rustc codegen cranelift will JIT run the executable, because -Cllvm-args=mode=jit was passed"
    );

    let args = ::std::env::var("CG_CLIF_JIT_ARGS").unwrap_or_else(|_| String::new());
    let args = std::iter::once(&*tcx.crate_name(LOCAL_CRATE).as_str().to_string())
        .chain(args.split(' '))
        .map(|arg| CString::new(arg).unwrap())
        .collect::<Vec<_>>();
    let mut argv = args.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();

    // Push a null pointer as a terminating argument. This is required by POSIX and
    // useful as some dynamic linkers use it as a marker to jump over.
    argv.push(std::ptr::null());

    BACKEND_CONFIG.with(|tls_backend_config| {
        assert!(tls_backend_config.borrow_mut().replace(backend_config).is_none())
    });

    let (main_def_id, entry_ty) = tcx.entry_fn(LOCAL_CRATE).unwrap();
    let instance = Instance::mono(tcx, main_def_id.to_def_id()).polymorphize(tcx);

    match entry_ty {
        EntryFnType::Main => {
            // FIXME set program arguments somehow

            let main_sig = Signature {
                params: vec![],
                returns: vec![],
                call_conv: CallConv::triple_default(&crate::target_triple(tcx.sess)),
            };
            let main_func_id = jit_module
                .declare_function(tcx.symbol_name(instance).name, Linkage::Import, &main_sig)
                .unwrap();
            let finalized_main: *const u8 = jit_module.get_finalized_function(main_func_id);

            CURRENT_MODULE.with(|current_module| {
                assert!(current_module.borrow_mut().replace(jit_module).is_none())
            });

            let f: extern "C" fn() = unsafe { ::std::mem::transmute(finalized_main) };
            f();
            std::process::exit(0);
        }
        EntryFnType::Start => {
            let start_sig = Signature {
                params: vec![
                    AbiParam::new(jit_module.target_config().pointer_type()),
                    AbiParam::new(jit_module.target_config().pointer_type()),
                ],
                returns: vec![AbiParam::new(
                    jit_module.target_config().pointer_type(), /*isize*/
                )],
                call_conv: CallConv::triple_default(&crate::target_triple(tcx.sess)),
            };
            let start_func_id = jit_module
                .declare_function(tcx.symbol_name(instance).name, Linkage::Import, &start_sig)
                .unwrap();
            let finalized_start: *const u8 = jit_module.get_finalized_function(start_func_id);

            CURRENT_MODULE.with(|current_module| {
                assert!(current_module.borrow_mut().replace(jit_module).is_none())
            });

            let f: extern "C" fn(c_int, *const *const c_char) -> c_int =
                unsafe { ::std::mem::transmute(finalized_start) };
            let ret = f(args.len() as c_int, argv.as_ptr());
            std::process::exit(ret);
        }
    }
}

#[no_mangle]
extern "C" fn __clif_jit_fn(instance_ptr: *const Instance<'static>) -> *const u8 {
    rustc_middle::ty::tls::with(|tcx| {
        // lift is used to ensure the correct lifetime for instance.
        let instance = tcx.lift(unsafe { *instance_ptr }).unwrap();

        CURRENT_MODULE.with(|jit_module| {
            let mut jit_module = jit_module.borrow_mut();
            let jit_module = jit_module.as_mut().unwrap();
            let backend_config =
                BACKEND_CONFIG.with(|backend_config| backend_config.borrow().clone().unwrap());

            let name = tcx.symbol_name(instance).name.to_string();
            let sig = crate::abi::get_function_sig(tcx, jit_module.isa().triple(), instance);
            let func_id = jit_module.declare_function(&name, Linkage::Export, &sig).unwrap();
            jit_module.prepare_for_function_redefine(func_id).unwrap();

            let mut cx = crate::CodegenCx::new(tcx, backend_config, jit_module, false);
            tcx.sess.time("codegen fn", || crate::base::codegen_fn(&mut cx, instance));

            let (global_asm, _debug_context, unwind_context) = cx.finalize();
            assert!(global_asm.is_empty());
            jit_module.finalize_definitions();
            std::mem::forget(unsafe { unwind_context.register_jit(&jit_module) });
            jit_module.get_finalized_function(func_id)
        })
    })
}

fn load_imported_symbols_for_jit(tcx: TyCtxt<'_>) -> Vec<(String, *const u8)> {
    use rustc_middle::middle::dependency_format::Linkage;

    let mut dylib_paths = Vec::new();

    let crate_info = CrateInfo::new(tcx);
    let formats = tcx.dependency_formats(LOCAL_CRATE);
    let data = &formats
        .iter()
        .find(|(crate_type, _data)| *crate_type == rustc_session::config::CrateType::Executable)
        .unwrap()
        .1;
    for &(cnum, _) in &crate_info.used_crates_dynamic {
        let src = &crate_info.used_crate_source[&cnum];
        match data[cnum.as_usize() - 1] {
            Linkage::NotLinked | Linkage::IncludedFromDylib => {}
            Linkage::Static => {
                let name = tcx.crate_name(cnum);
                let mut err =
                    tcx.sess.struct_err(&format!("Can't load static lib {}", name.as_str()));
                err.note("rustc_codegen_cranelift can only load dylibs in JIT mode.");
                err.emit();
            }
            Linkage::Dynamic => {
                dylib_paths.push(src.dylib.as_ref().unwrap().0.clone());
            }
        }
    }

    let mut imported_symbols = Vec::new();
    for path in dylib_paths {
        use object::{Object, ObjectSymbol};
        let lib = libloading::Library::new(&path).unwrap();
        let obj = std::fs::read(path).unwrap();
        let obj = object::File::parse(&obj).unwrap();
        imported_symbols.extend(obj.dynamic_symbols().filter_map(|symbol| {
            let name = symbol.name().unwrap().to_string();
            if name.is_empty() || !symbol.is_global() || symbol.is_undefined() {
                return None;
            }
            if name.starts_with("rust_metadata_") {
                // The metadata is part of a section that is not loaded by the dynamic linker in
                // case of cg_llvm.
                return None;
            }
            let dlsym_name = if cfg!(target_os = "macos") {
                // On macOS `dlsym` expects the name without leading `_`.
                assert!(name.starts_with('_'), "{:?}", name);
                &name[1..]
            } else {
                &name
            };
            let symbol: libloading::Symbol<'_, *const u8> =
                unsafe { lib.get(dlsym_name.as_bytes()) }.unwrap();
            Some((name, *symbol))
        }));
        std::mem::forget(lib)
    }

    tcx.sess.abort_if_errors();

    imported_symbols
}

fn codegen_shim<'tcx>(cx: &mut CodegenCx<'_, 'tcx>, inst: Instance<'tcx>) {
    let tcx = cx.tcx;

    let pointer_type = cx.module.target_config().pointer_type();

    let name = tcx.symbol_name(inst).name.to_string();
    let sig = crate::abi::get_function_sig(tcx, cx.module.isa().triple(), inst);
    let func_id = cx.module.declare_function(&name, Linkage::Export, &sig).unwrap();

    let instance_ptr = Box::into_raw(Box::new(inst));

    let jit_fn = cx
        .module
        .declare_function(
            "__clif_jit_fn",
            Linkage::Import,
            &Signature {
                call_conv: cx.module.target_config().default_call_conv,
                params: vec![AbiParam::new(pointer_type)],
                returns: vec![AbiParam::new(pointer_type)],
            },
        )
        .unwrap();

    let mut trampoline = Function::with_name_signature(ExternalName::default(), sig.clone());
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut trampoline_builder = FunctionBuilder::new(&mut trampoline, &mut builder_ctx);

    let jit_fn = cx.module.declare_func_in_func(jit_fn, trampoline_builder.func);
    let sig_ref = trampoline_builder.func.import_signature(sig);

    let entry_block = trampoline_builder.create_block();
    trampoline_builder.append_block_params_for_function_params(entry_block);
    let fn_args = trampoline_builder.func.dfg.block_params(entry_block).to_vec();

    trampoline_builder.switch_to_block(entry_block);
    let instance_ptr = trampoline_builder.ins().iconst(pointer_type, instance_ptr as u64 as i64);
    let jitted_fn = trampoline_builder.ins().call(jit_fn, &[instance_ptr]);
    let jitted_fn = trampoline_builder.func.dfg.inst_results(jitted_fn)[0];
    let call_inst = trampoline_builder.ins().call_indirect(sig_ref, jitted_fn, &fn_args);
    let ret_vals = trampoline_builder.func.dfg.inst_results(call_inst).to_vec();
    trampoline_builder.ins().return_(&ret_vals);

    cx.module
        .define_function(
            func_id,
            &mut Context::for_function(trampoline),
            &mut NullTrapSink {},
            &mut NullStackMapSink {},
        )
        .unwrap();
}
