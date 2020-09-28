-include ../../run-make-fulldeps/tools.mk

export RUSTC := $(RUSTC_ORIGINAL)

all:
	cd run && $$BOOTSTRAP_CARGO clean
	cd run && [ "`RUSTFLAGS='-C tracer=hw' $$BOOTSTRAP_CARGO test | grep 'result: ok' | wc -l `" -eq 1 ]
