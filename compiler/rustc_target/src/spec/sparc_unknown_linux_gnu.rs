use crate::abi::Endian;
use crate::spec::{LinkerFlavor, Target};

pub fn target() -> Target {
    let mut base = super::linux_gnu_base::opts();
    base.endian = Endian::Big;
    base.cpu = "v9".to_string();
    base.max_atomic_width = Some(64);
    base.pre_link_args.entry(LinkerFlavor::Gcc).or_default().push("-mv8plus".to_string());

    Target {
        llvm_target: "sparc-unknown-linux-gnu".to_string(),
        pointer_width: 32,
        data_layout: "E-m:e-p:32:32-i64:64-f128:64-n32-S64".to_string(),
        arch: "sparc".to_string(),
        options: base,
    }
}
