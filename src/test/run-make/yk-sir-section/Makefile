-include ../../run-make-fulldeps/tools.mk

all:
	# We unset YK_NO_SIR (if set) as this test really needs SIR.
	unset YK_NO_SIR && $(RUSTC) sir_sec.rs
	# Exit non-zero if there was no Yorick TIR section in the binary.
	[ "`readelf --section-headers ${TMPDIR}/sir_sec | grep yk_sir | wc -l`" != 0 ]
