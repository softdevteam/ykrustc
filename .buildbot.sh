#!/bin/sh
#
# Build script for continuous integration.

set -e

export PATH=PATH=/opt/gdb-8.2/bin:${PATH}

TARBALL_TOPDIR=`pwd`/build/ykrustc-stage2-latest
TARBALL_NAME=ykrustc-stage2-latest.tar.bz2
SNAP_DIR=/opt/ykrustc-bin-snapshots

# Ensure the build fails if it uses excessive amounts of memory.
ulimit -d $((1024 * 1024 * 8)) # 8 GiB

# Note that the gdb must be Python enabled.
/usr/bin/time -v ./x.py test --config .buildbot.config.toml

# Build extended tools and install into TARBALL_TOPDIR.
mkdir -p ${TARBALL_TOPDIR}
/usr/bin/time -v ./x.py install --config .buildbot.config.toml

# Archive the build and put it in /opt
git show -s HEAD > ${TARBALL_TOPDIR}/VERSION
cd build
tar jcf ${TARBALL_NAME} `basename ${TARBALL_TOPDIR}`
chmod 775 ${TARBALL_NAME}
mv ${TARBALL_NAME} ${SNAP_DIR} # Overwrites any old archive.
