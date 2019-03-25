#!/bin/sh
#
# Build script for continuous integration.

./x.py clean  # We don't clone afresh to save time and bandwidth.
git clean -dffx # If upstream removes a submodule, remove the files from disk.

# Ensure the build fails if it uses excessive amounts of memory.
ulimit -d $((1024 * 1024 * 8)) # 8 GiB

# Note that the gdb must be Python enabled.
/usr/bin/time -v env PATH=/opt/gdb-8.2/bin:${PATH} \
    RUST_BACKTRACE=1 ./x.py test --config .buildbot.toml
