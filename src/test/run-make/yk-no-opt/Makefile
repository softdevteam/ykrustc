-include ../../run-make-fulldeps/tools.mk

PROG=yk-no-opt

all:
	$(RUSTC) -C tracer=hw ${PROG}.rs
	$(RUSTC) -C tracer=hw -C opt-level=0 ${PROG}.rs

	$(RUSTC) -C tracer=hw -C opt-level=1 ${PROG}.rs; [ $$? -ne 0 ]
	$(RUSTC) -C tracer=hw -C opt-level=2 ${PROG}.rs; [ $$? -ne 0 ]
	$(RUSTC) -C tracer=hw -C opt-level=s ${PROG}.rs; [ $$? -ne 0 ]
