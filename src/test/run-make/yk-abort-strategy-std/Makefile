-include ../../run-make-fulldeps/tools.mk

PROG=yk-abort-strategy-std

all:
	$(RUSTC) --emit mir ${PROG}.rs && grep 'resume;' ${TMPDIR}/${PROG}.mir
	$(RUSTC) --emit mir -C tracer=hw ${PROG}.rs && grep 'resume;' ${TMPDIR}/${PROG}.mir; [ $$? -eq 1 ]
