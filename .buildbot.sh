#!/bin/sh
#
# Build script for continuous integration.

set -e

# Ensure the build fails if it uses excessive amounts of memory.
ulimit -d $((1024 * 1024 * 8)) # 8 GiB

# Note that the gdb must be Python enabled.
/usr/bin/time -v env PATH=/opt/gdb-8.2/bin:${PATH} \
    RUST_BACKTRACE=1 ./x.py test --config .buildbot.toml

# Archive the build and put it in /opt
TARBALL_TOPDIR=ykrustc-stage2
TARBALL_NAME=ykrustc-stage2-latest.tar.bz2
SNAP_DIR=/opt/ykrustc-bin-snapshots

cd build/x86_64-unknown-linux-gnu
ln -sf stage2 ${TARBALL_TOPDIR}
git show -s HEAD > ${TARBALL_TOPDIR}/VERSION
tar hjcvf ${TARBALL_NAME} ${TARBALL_TOPDIR}
chmod 775 ${TARBALL_NAME}
mv ${TARBALL_NAME} ${SNAP_DIR} # Overwrites any old archive.
