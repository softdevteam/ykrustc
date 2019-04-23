#!/bin/sh
#
# Build script for continuous integration.

# Ensure the build fails if it uses excessive amounts of memory.
ulimit -d $((1024 * 1024 * 8)) # 8 GiB

# Note that the gdb must be Python enabled.
/usr/bin/time -v env PATH=/opt/gdb-8.2/bin:${PATH} \
    RUST_BACKTRACE=1 ./x.py test --config .buildbot.toml
