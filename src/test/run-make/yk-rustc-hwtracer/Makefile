-include ../../run-make-fulldeps/tools.mk

all:
	$(RUSTC) -C tracer=hw block.rs
	[ "`dwarfdump ${TMPDIR}/block | grep __YK_BLK | wc -l`" -ne 0 ]

	$(RUSTC) -C tracer=sw block.rs
	[ "`dwarfdump ${TMPDIR}/block | grep __YK_BLK | wc -l`" -eq 0 ]
