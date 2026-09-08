#define _XOPEN_SOURCE 700
#include <assert.h>
#include <errno.h>
#include <ftw.h>
#include <limits.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* The build extracts these functions from the actual patched runtime.c. */
static bool inject_noop;
static int test_nftw(const char *path,
    int (*callback)(const char *, const struct stat *, int, struct FTW *),
    int descriptors, int flags)
{
    return inject_noop ? 0 : nftw(path, callback, descriptors, flags);
}
#define nftw test_nftw
#include "runtime-cleanup-functions.inc"
#undef nftw

static void check(const char *name, int depth, bool noop, bool read_only)
{
    char root[] = "/tmp/gfs-runtime-cleanup-XXXXXX";
    char sentinel[] = "/tmp/gfs-runtime-sentinel-XXXXXX";
    char path[PATH_MAX];
    int descriptor = mkstemp(sentinel);
    assert(descriptor >= 0);
    assert(write(descriptor, "keep", 4) == 4);
    assert(close(descriptor) == 0);
    assert(mkdtemp(root));
    assert(snprintf(path, sizeof path, "%s/outside", root) > 0);
    assert(symlink(sentinel, path) == 0);
    assert(snprintf(path, sizeof path, "%s", root) > 0);
    for (int index = 0; index < depth; index++) {
        assert(strlen(path) + 3 < sizeof path);
        strcat(path, "/n");
        assert(mkdir(path, 0700) == 0);
    }
    strcat(path, "/leaf");
    FILE *file = fopen(path, "w");
    assert(file && fclose(file) == 0);
    if (read_only)
        assert(chmod(root, 0500) == 0);
    inject_noop = noop;
    bool result = rm_recursive(root);
    inject_noop = false;
    struct stat info;
    bool remains = lstat(root, &info) == 0;
    bool expected_failure = noop || read_only || depth >= 64;
    printf("case=%s success=%d root_remaining=%d expected_failure=%d\n",
        name, result, remains, expected_failure);
    assert(result != expected_failure && remains == expected_failure);
    char bytes[4];
    file = fopen(sentinel, "r");
    assert(file && fread(bytes, 1, sizeof bytes, file) == 4);
    assert(memcmp(bytes, "keep", 4) == 0 && fclose(file) == 0);
    if (remains) {
        assert(chmod(root, 0700) == 0);
        /* Remove only this intentionally over-deep/failed test fixture. */
        assert(nftw(root, rm_recursive_callback, 256,
            FTW_DEPTH | FTW_MOUNT | FTW_PHYS) == 0);
    }
    assert(lstat(root, &info) == -1 && errno == ENOENT);
    assert(unlink(sentinel) == 0);
}

int main(void)
{
    assert(geteuid() != 0); /* Permission-failure testing must not bypass DAC. */
    check("shallow", 3, false, false);
    check("deep-reported", 70, false, false);
    check("postcondition-noop", 1, true, false);
    check("permission-reported", 1, false, true);
    puts("patched_runtime_cleanup=PASS external_symlinks=preserved fixtures=removed");
    return 0;
}
