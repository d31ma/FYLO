/*
 * Links the fylo-ffi static library from C and runs one plain database
 * through every call of include/fylo.h (ADR 0026). Run it with
 * `bun run test:ffi`. Exit status 0 means every check passed.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "fylo.h"

static int failures = 0;

static void expect(int condition, const char *what) {
    if (!condition) {
        fprintf(stderr, "FAIL %s\n", what);
        failures += 1;
    }
}

static const uint8_t *text(const char *value) {
    return (const uint8_t *)value;
}

static int holds(FyloBytes bytes, const char *part) {
    char *copy = malloc(bytes.len + 1);
    memcpy(copy, bytes.ptr, bytes.len);
    copy[bytes.len] = '\0';
    int found = strstr(copy, part) != NULL;
    free(copy);
    return found;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: smoke <path of a new .vdbx file>\n");
        return 2;
    }
    const char *path = argv[1];
    const char *options = "{\"create\": {\"mode\": \"plain\"}}";
    uint64_t root = 0;
    FyloBytes out = {0};

    expect(fylo_abi_version() == FYLO_ABI_VERSION, "abi version");

    FyloStatus status = fylo_open(text(path), strlen(path), text(options), strlen(options),
                                  NULL, 0, &root, &out);
    expect(status == FYLO_OK && root != 0, "open creates the database");
    fylo_bytes_free(out);

    status = fylo_create_collection(root, text("notes"), 5, &out);
    expect(status == FYLO_OK && holds(out, "\"notes\""), "create collection");
    fylo_bytes_free(out);

    const char *document = "{\"title\": \"from C\", \"rank\": 1}";
    status = fylo_put(root, text("notes"), 5, text(document), strlen(document), &out);
    expect(status == FYLO_OK && out.len > 2, "put returns the id");
    char id[64] = {0};
    if (out.len > 2 && out.len - 2 < sizeof id) {
        memcpy(id, out.ptr + 1, out.len - 2);
    }
    fylo_bytes_free(out);

    status = fylo_get(root, text("notes"), 5, text(id), strlen(id), &out);
    expect(status == FYLO_OK && holds(out, "from C"), "get reads the document");
    fylo_bytes_free(out);

    const char *query = "{\"$ops\": [{\"title\": {\"$eq\": \"from C\"}}]}";
    status = fylo_find(root, text("notes"), 5, text(query), strlen(query), &out);
    expect(status == FYLO_OK && holds(out, id), "find matches the document");
    fylo_bytes_free(out);

    const char *page = "{\"$sort\": {\"rank\": \"asc\"}, \"$limit\": 10}";
    status = fylo_find_page(root, text("notes"), 5, text(page), strlen(page), &out);
    expect(status == FYLO_OK && holds(out, "\"nextCursor\"") && holds(out, id),
           "find_page returns a page");
    fylo_bytes_free(out);

    status = fylo_delete(root, text("notes"), 5, text(id), strlen(id), &out);
    expect(status == FYLO_OK && holds(out, "\"deleted\""), "delete");
    fylo_bytes_free(out);

    status = fylo_get(root, text("notes"), 5, text(id), strlen(id), &out);
    expect(status == FYLO_NOT_FOUND, "a deleted document is not found");
    fylo_bytes_free(out);

    status = fylo_close(root, &out);
    expect(status == FYLO_OK, "close");
    fylo_bytes_free(out);

    status = fylo_get(root, text("notes"), 5, text(id), strlen(id), NULL);
    expect(status == FYLO_CLOSED, "a closed handle answers FYLO_CLOSED");

    if (failures == 0) {
        printf("fylo C smoke: all checks passed\n");
    }
    return failures == 0 ? 0 : 1;
}
