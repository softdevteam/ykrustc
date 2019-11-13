-include ../../run-make-fulldeps/tools.mk

all:
	$(RUSTC) -C tracer=sw sw.rs -o sw
	./sw
	$(RUSTC) -C tracer=hw hw.rs -o hw
	./hw
	$(RUSTC) -C tracer=off off.rs -o off
	./off
