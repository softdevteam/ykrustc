//! Drivers are responsible for calling [`codegen_mono_item`] and performing any further actions
//! like JIT executing or writing object files.

use std::any::Any;

use rustc_middle::middle::cstore::EncodedMetadata;
use rustc_middle::mir::mono::{Linkage as RLinkage, MonoItem, Visibility};

use crate::prelude::*;
use crate::CodegenMode;

mod aot;
#[cfg(feature = "jit")]
mod jit;

pub(crate) fn codegen_crate(
    tcx: TyCtxt<'_>,
    metadata: EncodedMetadata,
    need_metadata_module: bool,
    backend_config: crate::BackendConfig,
) -> Box<dyn Any> {
    tcx.sess.abort_if_errors();

    match backend_config.codegen_mode {
        CodegenMode::Aot => aot::run_aot(tcx, backend_config, metadata, need_metadata_module),
        CodegenMode::Jit | CodegenMode::JitLazy => {
            let is_executable =
                tcx.sess.crate_types().contains(&rustc_session::config::CrateType::Executable);
            if !is_executable {
                tcx.sess.fatal("can't jit non-executable crate");
            }

            #[cfg(feature = "jit")]
            let _: ! = jit::run_jit(tcx, backend_config);

            #[cfg(not(feature = "jit"))]
            tcx.sess.fatal("jit support was disabled when compiling rustc_codegen_cranelift");
        }
    }
}

fn predefine_mono_items<'tcx>(
    cx: &mut crate::CodegenCx<'_, 'tcx>,
    mono_items: &[(MonoItem<'tcx>, (RLinkage, Visibility))],
) {
    cx.tcx.sess.time("predefine functions", || {
        let is_compiler_builtins = cx.tcx.is_compiler_builtins(LOCAL_CRATE);
        for &(mono_item, (linkage, visibility)) in mono_items {
            match mono_item {
                MonoItem::Fn(instance) => {
                    let name = cx.tcx.symbol_name(instance).name.to_string();
                    let _inst_guard = crate::PrintOnPanic(|| format!("{:?} {}", instance, name));
                    let sig = get_function_sig(cx.tcx, cx.module.isa().triple(), instance);
                    let linkage = crate::linkage::get_clif_linkage(
                        mono_item,
                        linkage,
                        visibility,
                        is_compiler_builtins,
                    );
                    cx.module.declare_function(&name, linkage, &sig).unwrap();
                }
                MonoItem::Static(_) | MonoItem::GlobalAsm(_) => {}
            }
        }
    });
}

fn time<R>(tcx: TyCtxt<'_>, name: &'static str, f: impl FnOnce() -> R) -> R {
    if std::env::var("CG_CLIF_DISPLAY_CG_TIME").as_ref().map(|val| &**val) == Ok("1") {
        println!("[{:<30}: {}] start", tcx.crate_name(LOCAL_CRATE), name);
        let before = std::time::Instant::now();
        let res = tcx.sess.time(name, f);
        let after = std::time::Instant::now();
        println!("[{:<30}: {}] end time: {:?}", tcx.crate_name(LOCAL_CRATE), name, after - before);
        res
    } else {
        tcx.sess.time(name, f)
    }
}
