-include ../../run-make-fulldeps/tools.mk

all:
	$(RUSTC) tir_sec.rs
	# Exit non-zero if there was no Yorick TIR section in the binary.
	[ "`readelf --section-headers ${TMPDIR}/tir_sec | grep yk_tir | wc -l`" != 0 ]
