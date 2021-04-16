use crate::spec::{LinkerFlavor, StackProbeType, Target};

pub fn target() -> Target {
    let mut base = super::solaris_base::opts();
    base.pre_link_args.insert(LinkerFlavor::Gcc, vec!["-m64".to_string()]);
    base.cpu = "x86-64".to_string();
    base.vendor = "sun".to_string();
    base.max_atomic_width = Some(64);
    base.stack_probes = StackProbeType::InlineOrCall { min_llvm_version_for_inline: (11, 0, 1) };

    Target {
        llvm_target: "x86_64-pc-solaris".to_string(),
        pointer_width: 64,
        data_layout: "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128"
            .to_string(),
        arch: "x86_64".to_string(),
        options: base,
    }
}
