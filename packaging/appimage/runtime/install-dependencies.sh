#!/bin/bash
set -euo pipefail
export LC_ALL=C

gfs_input=/tmp/gfs-sdk-input
gfs_sdk=/gfs-sdk

# No downloads here. The Docker context contains the exact upstream inputs.
cd "$gfs_input"
printf '%s\n' \
    '70589cfd5e1cff7ccd6ac91c86c01be340b227285c5e200baa284e401eea2ca0  fuse-3.15.0.tar.xz' \
    'db0238c5981dabbd80ee09ae15387f390091668ca060a7bc38047912491443d3  squashfuse-0.5.2.tar.gz' \
    '1c7fd9e26717545a476b226b083a9f9d05676c180edbd71a04bbd8a73599dc44  mount.c.diff' \
    | sha256sum -c -

mkdir -p "$gfs_sdk/sources" "$gfs_sdk/metadata"
cp fuse-3.15.0.tar.xz squashfuse-0.5.2.tar.gz mount.c.diff "$gfs_sdk/sources/"
cp install-dependencies.sh "$gfs_sdk/metadata/"
sha256sum fuse-3.15.0.tar.xz squashfuse-0.5.2.tar.gz mount.c.diff \
    > "$gfs_sdk/sources/SHA256SUMS"
printf '%s\n' \
    'fuse-3.15.0.tar.xz https://github.com/libfuse/libfuse/releases/download/fuse-3.15.0/fuse-3.15.0.tar.xz' \
    'squashfuse-0.5.2.tar.gz https://github.com/vasi/squashfuse/archive/0.5.2.tar.gz' \
    'mount.c.diff https://github.com/AppImage/type2-runtime/blob/dd6cebedcbddde9c82f89b011e8e1d40b6e43868/patches/libfuse/mount.c.diff' \
    > "$gfs_sdk/sources/SOURCES.txt"

gfs_build_dir=$(mktemp -d /tmp/gfs-sdk-build.XXXXXXXX)
gfs_cleanup() {
    # Only the directory allocated by this invocation, never an unresolved root.
    if [[ "$gfs_build_dir" == /tmp/gfs-sdk-build.* && -d "$gfs_build_dir" && ! -L "$gfs_build_dir" ]]; then
        rm -rf -- "$gfs_build_dir"
    fi
}
trap gfs_cleanup EXIT

tar -xJf "$gfs_sdk/sources/fuse-3.15.0.tar.xz" -C "$gfs_build_dir"
cd "$gfs_build_dir/fuse-3.15.0"
patch --batch --forward -p1 < "$gfs_sdk/sources/mount.c.diff"
mkdir build
cd build
meson setup --prefix=/usr ..
meson configure --default-library static
meson configure > "$gfs_sdk/metadata/libfuse-meson-config.txt"
ninja -j2 -v install > "$gfs_sdk/metadata/libfuse-build.log" 2>&1
cp meson-logs/meson-log.txt "$gfs_sdk/metadata/libfuse-meson-log.txt"

export CFLAGS='-ffunction-sections -fdata-sections -Os'
tar -xzf "$gfs_sdk/sources/squashfuse-0.5.2.tar.gz" -C "$gfs_build_dir"
cd "$gfs_build_dir/squashfuse-0.5.2"
./autogen.sh > "$gfs_sdk/metadata/squashfuse-autogen.log" 2>&1
./configure LDFLAGS='-static' > "$gfs_sdk/metadata/squashfuse-configure.log" 2>&1
make -j2 > "$gfs_sdk/metadata/squashfuse-build.log" 2>&1
make install >> "$gfs_sdk/metadata/squashfuse-build.log" 2>&1
install -c -m 644 ./*.h /usr/local/include/squashfuse
cp config.log config.status "$gfs_sdk/metadata/"

cp /etc/alpine-release "$gfs_sdk/metadata/alpine-release"
cp /etc/apk/repositories "$gfs_sdk/metadata/apk-repositories"
cp /lib/apk/db/installed "$gfs_sdk/metadata/apk-installed"
apk info -v | sort > "$gfs_sdk/metadata/apk-packages.txt"
apk --print-arch > "$gfs_sdk/metadata/apk-architecture.txt"
{
    printf '%s\n' '=== clang ==='
    clang --version
    printf '%s\n' '=== linker ==='
    ld --version
    printf '%s\n' '=== ar / objcopy / strip ==='
    ar --version
    objcopy --version
    strip --version
    printf '%s\n' '=== build tools ==='
    make --version
    meson --version
    ninja --version
    printf '%s\n' '=== compiler search ==='
    clang -print-search-dirs
    clang -print-libgcc-file-name
} > "$gfs_sdk/metadata/toolchain.txt"
printf '%s\n' \
    'Source inputs: AppImage/type2-runtime dd6cebedcbddde9c82f89b011e8e1d40b6e43868.' \
    'libfuse 3.15.0 original archive and upstream mount.c.diff are preserved.' \
    'squashfuse 0.5.2 original archive and build configuration are preserved.' \
    'PENDING: exact Alpine APK corresponding sources, patches and licenses for musl,' \
    'zstd, zlib, mimalloc and any compiler-support/CRT inputs identified by the link map.' \
    'APK identities/build metadata and binary archives do not replace corresponding sources.' \
    'This SDK does not establish complete redistribution compliance or hermetic compilation.' \
    > "$gfs_sdk/metadata/SOURCE-CLOSURE-PENDING.txt"
chmod -R a+rX "$gfs_sdk"
