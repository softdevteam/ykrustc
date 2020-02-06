-include ../../run-make-fulldeps/tools.mk

all:
	# A tracer must be enabled for SIR to be emitted.
	$(RUSTC) -C tracer=sw sir_sec.rs
	# Exit non-zero if there was no Yorick SIR section in the binary.
	[ "`readelf --section-headers ${TMPDIR}/sir_sec | grep yksir | wc -l`" != 0 ]
