FROM alpine:3.21@sha256:48b0309ca019d89d40f670aa1bc06e426dc0931948452e8491e3d65087abc07d

# Match the fixed upstream SDK's dependency set. Exact installed APK identities
# are exported to /gfs-sdk; apk repositories are not claimed hermetic/pinned.
RUN apk add --no-cache \
    bash alpine-sdk util-linux strace file autoconf automake libtool xz \
    eudev-dev gettext-dev linux-headers meson \
    zstd-dev zstd-static zlib-dev zlib-static clang musl-dev mimalloc-dev

COPY install-dependencies.sh fuse-3.15.0.tar.xz squashfuse-0.5.2.tar.gz mount.c.diff /tmp/gfs-sdk-input/
RUN bash /tmp/gfs-sdk-input/install-dependencies.sh

# Compilation is invoked explicitly by the outer supervisor as a non-root UID,
# with read-only /ws, writable /out, private /tmp, and no network/capabilities.
WORKDIR /ws
