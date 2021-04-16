use crate::spec::{RelroLevel, TargetOptions};

pub fn opts() -> TargetOptions {
    TargetOptions {
        os: "openbsd".to_string(),
        dynamic_linking: true,
        executables: true,
        os_family: Some("unix".to_string()),
        linker_is_gnu: true,
        has_rpath: true,
        abi_return_struct_as_int: true,
        position_independent_executables: true,
        eliminate_frame_pointer: false, // FIXME 43575
        relro_level: RelroLevel::Full,
        dwarf_version: Some(2),
        ..Default::default()
    }
}
