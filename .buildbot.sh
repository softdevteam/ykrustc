#!/bin/sh
#
# Build script for continuous integration.

set -ex

export PATH=PATH=/opt/gdb-8.2/bin:${PATH}

COMMIT_HASH=$(git rev-parse --short HEAD)
TARBALL_TOPDIR=`pwd`/build/ykrustc-stage2-latest
TARBALL_NAME=ykrustc-${STD_TRACER_MODE}-stage2-${COMMIT_HASH}.tar.bz2
SYMLINK_NAME=ykrustc-${STD_TRACER_MODE}-stage2-latest.tar.bz2
SNAP_DIR=/opt/ykrustc-bin-snapshots

# Ensure the build fails if it uses excessive amounts of memory.
ulimit -d $((1024 * 1024 * 10)) # 10 GiB

# Patch the yk dependency if necessary.
# This step requires the 'github3.py' python module.
git show HEAD # XXX debug
pr_no=`git show HEAD | awk  '/(Merge #|Try #)/ {gsub(/ /, "", $2); gsub (/:/, "", $2); print substr($2, 1, length($2) - 1)}'`
/opt/buildbot/bin/python3 .buildbot_patch_yk_dep.py ${pr_no}
exit 1

# Note that the gdb must be Python enabled.
/usr/bin/time -v ./x.py test --config .buildbot.config.toml --stage 2

# Build extended tools and install into TARBALL_TOPDIR.
mkdir -p ${TARBALL_TOPDIR}
/usr/bin/time -v ./x.py install --config .buildbot.config.toml

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
