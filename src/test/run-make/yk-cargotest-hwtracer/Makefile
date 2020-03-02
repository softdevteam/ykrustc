-include ../../run-make-fulldeps/tools.mk

export RUSTC := $(RUSTC_ORIGINAL)

all:
	cd run && $$CARGO clean
	cd run && [ "`RUSTFLAGS='-C tracer=hw' $$CARGO test | grep 'result: ok' | wc -l `" -eq 1 ]
