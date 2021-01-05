#!/bin/sh
#
# Build script for continuous integration.

set -e

export PATH=PATH=/opt/gdb-8.2/bin:${PATH}

COMMIT_HASH=$(git rev-parse --short HEAD)
TARBALL_TOPDIR=`pwd`/build/ykrustc-stage2-latest
TARBALL_NAME=ykrustc-${STD_TRACER_MODE}-stage2-${COMMIT_HASH}.tar.bz2
SYMLINK_NAME=ykrustc-${STD_TRACER_MODE}-stage2-latest.tar.bz2
SNAP_DIR=/opt/ykrustc-bin-snapshots
PLATFORM_BUILD_DIR=build/x86_64-unknown-linux-gnu

# Ensure the build fails if it uses excessive amounts of memory.
ulimit -d $((1024 * 1024 * 10)) # 10 GiB

# Check for some common developer mistakes.
/opt/buildbot/bin/python3 .buildbot_prechecks.py

# Patch the yk dependency if necessary.
# This step requires the 'github3.py' module.
/opt/buildbot/bin/python3 .buildbot_patch_yk_dep.py

# Note that the gdb must be Python enabled.
/usr/bin/time -v ./x.py test --config .buildbot.config.toml --stage 2

# Build extended tools and install into TARBALL_TOPDIR.
mkdir -p ${TARBALL_TOPDIR}
# `x.py install` is currently broken, so we use a workaround for now.
# https://github.com/rust-lang/rust/issues/80683
#/usr/bin/time -v ./x.py install --config .buildbot.config.toml
/usr/bin/time -v ./x.py build --stage 2 --config .buildbot.config.toml library/std rustdoc cargo
cp -r ${PLATFORM_BUILD_DIR}/stage2/* ${TARBALL_TOPDIR}
cp -r ${PLATFORM_BUILD_DIR}/stage2-tools-bin/cargo ${TARBALL_TOPDIR}/bin

# Archive the build and put it in /opt
git show -s HEAD > ${TARBALL_TOPDIR}/VERSION
cd build
tar jcf ${TARBALL_NAME} `basename ${TARBALL_TOPDIR}`
chmod 775 ${TARBALL_NAME}
mv ${TARBALL_NAME} ${SNAP_DIR}
ln -sf ${SNAP_DIR}/${TARBALL_NAME} ${SNAP_DIR}/${SYMLINK_NAME}

# Remove all but the 10 latest builds
cd ${SNAP_DIR}
sh -c "ls -tp | grep -v '/$' | tail -n +11 | xargs -I {} rm -- {}"
