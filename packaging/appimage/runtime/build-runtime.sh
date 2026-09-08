#!/bin/bash
set -euo pipefail
export LC_ALL=C

[[ -d /ws/src/runtime && -f /ws/src/runtime/version ]] || {
    printf '%s\n' 'Expected read-only /ws/src/runtime and an explicit version file.' >&2
    exit 1
}
[[ -d /out && ! -L /out && -w /out ]] || {
    printf '%s\n' 'Expected a fresh writable /out directory.' >&2
    exit 1
}
[[ -d /gfs-sdk/metadata ]] || {
    printf '%s\n' 'The fixed runtime SDK metadata is unavailable.' >&2
    exit 1
}
if [[ -n "$(find /out -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
    printf '%s\n' '/out is not empty; refusing to overwrite a prior build.' >&2
    exit 1
fi
if ! file -L /bin/bash | grep -q 'x86-64'; then
    printf '%s\n' 'This runtime build is limited to Linux x86_64.' >&2
    exit 1
fi
if [[ -n "$(find /ws/src /gfs-sdk ! -type f ! -type d -print -quit)" ]]; then
    printf '%s\n' 'Source/SDK material contains a link or special file; refusing to export it.' >&2
    exit 1
fi

gfs_work=$(mktemp -d /tmp/gfs-runtime-build.XXXXXXXX)
gfs_relink=/out/relink
mkdir -p "$gfs_relink/source" "$gfs_relink/sysroot" "$gfs_relink/sdk"
cp -R /ws/src "$gfs_work/src"
cp -R /ws/src "$gfs_relink/source/src"
if [[ -f /ws/LICENSE ]]; then
    cp /ws/LICENSE "$gfs_relink/source/LICENSE"
fi
cp /ws/build-runtime.sh "$gfs_relink/source/build-runtime.sh"
cp -R /gfs-sdk/. "$gfs_relink/sdk/"

cd "$gfs_work/src/runtime"
# Exercise the exact patched functions, not a separately maintained copy.
awk '
    /^int rm_recursive_callback\(/ { if (started++) exit 1; copying = 1 }
    /^bool rm_recursive\(/ { if (copying) recursive++ }
    /^void build_mount_point\(/ { if (copying) { copying = 0; finished++ } }
    copying { print }
    END { if (started != 1 || recursive != 1 || finished != 1) exit 1 }
' runtime.c > runtime-cleanup-functions.inc
[[ -s runtime-cleanup-functions.inc && -f /ws/cleanup-test.c ]] || exit 1
cp /ws/cleanup-test.c "$gfs_relink/cleanup-test.c"
cp runtime-cleanup-functions.inc "$gfs_relink/runtime-cleanup-functions.inc"
clang -std=gnu99 -Wall -Werror -D_FILE_OFFSET_BITS=64 -I. \
    /ws/cleanup-test.c -o "$gfs_work/cleanup-test"
"$gfs_work/cleanup-test" 2>&1 | tee /out/cleanup-test.log
cp /out/cleanup-test.log "$gfs_relink/cleanup-test.log"

# Append to, rather than replacing, the fixed upstream flags. Retain the real
# relocatable runtime.o, dependency headers and complete linker input evidence.
printf '%s\n' \
    'override CFLAGS += -save-temps=obj -MD -MF runtime.d -Wl,-Map,runtime.map -Wl,--trace' \
    > gfs-build-flags.mk
make -n -f Makefile -f gfs-build-flags.mk runtime > "$gfs_relink/build-command.txt"
make -j2 -f Makefile -f gfs-build-flags.mk runtime 2>&1 | tee "$gfs_relink/build.log"
[[ -s runtime.o && -s runtime.map && -s runtime.d ]] || {
    printf '%s\n' 'Compiler did not retain the required relink materials.' >&2
    exit 1
}
readelf --file-header runtime.o > "$gfs_relink/runtime-object-elf.txt"
grep -q 'REL (Relocatable file)' "$gfs_relink/runtime-object-elf.txt" || {
    printf '%s\n' 'runtime.o is not a relocatable object.' >&2
    exit 1
}
readelf --file-header runtime > "$gfs_relink/runtime-elf.txt"
grep -q 'Advanced Micro Devices X86-64' "$gfs_relink/runtime-elf.txt" || exit 1
readelf --dynamic --wide runtime > "$gfs_relink/runtime-dynamic.txt"
if grep -q '(NEEDED)' "$gfs_relink/runtime-dynamic.txt"; then
    printf '%s\n' 'Runtime unexpectedly requires a shared library.' >&2
    exit 1
fi
cp runtime.o runtime.map runtime.d Makefile data_sections.ld version gfs-build-flags.mk "$gfs_relink/"

# Compiler -MD supplies the actual header closure, including system headers.
# All compiler paths here are SDK paths without whitespace; refuse ambiguous
# dependency escapes rather than silently claim an incomplete copied closure.
if grep -q '\\ ' runtime.d; then
    printf '%s\n' 'Unexpected escaped whitespace in compiler dependency paths.' >&2
    exit 1
fi
awk '{ sub(/\\$/, ""); for (i=1; i<=NF; i++) if ($i ~ /^\//) print $i }' runtime.d \
    | sort -u > "$gfs_relink/header-paths.txt"
# GNU/LLVM link maps and --trace identify actual archives and CRT objects. The
# runtime's own relative runtime.o is already retained separately above.
awk '{ for (i=1; i<=NF; i++) { p=$i; sub(/\(.*/, "", p); if (p ~ /^\// && p ~ /\.(a|o)$/) print p } }' \
    runtime.map "$gfs_relink/build.log" | sort -u > "$gfs_relink/link-input-paths.txt"
[[ -s "$gfs_relink/header-paths.txt" && -s "$gfs_relink/link-input-paths.txt" ]] || {
    printf '%s\n' 'No header/archive closure was found; relink evidence is incomplete.' >&2
    exit 1
}
printf '%s\n' '# Original absolute path | resolved path | package ownership' > "$gfs_relink/package-owners.txt"
while IFS= read -r gfs_path; do
    [[ "$gfs_path" == /usr/* || "$gfs_path" == /lib/* ]] || {
        printf 'Unexpected compiler dependency outside the SDK: %s\n' "$gfs_path" >&2
        exit 1
    }
    [[ -f "$gfs_path" ]] || {
        printf 'Missing actual compiler dependency: %s\n' "$gfs_path" >&2
        exit 1
    }
    gfs_resolved=$(readlink -f "$gfs_path")
    [[ "$gfs_resolved" == /usr/* || "$gfs_resolved" == /lib/* ]] || {
        printf 'Compiler input resolved outside the SDK: %s\n' "$gfs_path" >&2
        exit 1
    }
    gfs_target="$gfs_relink/sysroot$gfs_resolved"
    mkdir -p "$(dirname "$gfs_target")"
    cp -L "$gfs_path" "$gfs_target"
    printf '%s | %s | ' "$gfs_path" "$gfs_resolved" >> "$gfs_relink/package-owners.txt"
    if ! apk info --who-owns "$gfs_path" >> "$gfs_relink/package-owners.txt" 2>&1; then
        printf '%s\n' 'Not APK-owned: consult pinned libfuse/squashfuse source/build records.' \
            >> "$gfs_relink/package-owners.txt"
    fi
done < <(sort -u "$gfs_relink/header-paths.txt" "$gfs_relink/link-input-paths.txt")

# Match the official post-link sequence. Debug info is retained in addition to
# the relocatable object, never used as a substitute for that object.
objcopy --only-keep-debug runtime runtime.debug
strip --strip-debug --strip-unneeded runtime
mv runtime runtime-x86_64
mv runtime.debug runtime-x86_64.debug
objcopy --add-gnu-debuglink runtime-x86_64.debug runtime-x86_64
printf 'AI\002' | dd of=runtime-x86_64 bs=1 count=3 seek=8 conv=notrunc
cp runtime-x86_64 runtime-x86_64.debug /out/
chmod 755 /out/runtime-x86_64
chmod 644 /out/runtime-x86_64.debug

{
    printf '%s\n' 'LOCAL PATCHED RUNTIME BUILD — SOURCE/REDISTRIBUTION CLOSURE PENDING'
    printf '%s\n' 'Upstream commit: dd6cebedcbddde9c82f89b011e8e1d40b6e43868'
    printf 'Built version: '; tr '\n' ' ' < version; printf '\n'
    printf '%s\n' 'Architecture: x86_64; link mode: static PIE; build parallelism: 2'
    printf '%s\n' 'Compiler:'; clang --version
    printf '%s\n' 'Actual link inputs:'; cat "$gfs_relink/link-input-paths.txt"
    printf '%s\n' 'Source and final output hashes:'
    sha256sum /ws/src/runtime/runtime.c /ws/src/runtime/Makefile /ws/src/runtime/data_sections.ld \
        /ws/src/runtime/version /ws/build-runtime.sh /out/runtime-x86_64 /out/runtime-x86_64.debug
    printf '%s\n' 'Outer receipt must add source archive/patch hashes and exact SDK image identity.'
    printf '%s\n' 'See relink/sdk/metadata/SOURCE-CLOSURE-PENDING.txt before redistribution.'
} > /out/build-details.txt
printf '%s\n' \
    'Relink materials: runtime.o, Makefile, linker script, exact command/map, headers,' \
    'actual static archives/CRT files and SDK package records. Re-run the recorded' \
    'compile/link command in the recorded SDK, substituting a rebuilt library as needed.' \
    'Then apply objcopy --only-keep-debug, strip, --add-gnu-debuglink and the AI02 marker' \
    'in the same order as source/build-runtime.sh. Debug ELF is not a relocatable object.' \
    'This retention is not itself a verified offline rebuild or complete corresponding-source offer.' \
    > "$gfs_relink/README.txt"
printf '%s\n' 'Built /out/runtime-x86_64 and retained relink materials; redistribution remains pending.'
