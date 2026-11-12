/*
 * FYLO's C ABI (ADR 0026). The library is built from crates/fylo-ffi: a
 * static library for iOS and a shared library for Android.
 *
 * Inputs are a pointer and a length (UTF-8 JSON or text, no NUL needed),
 * borrowed for the call only. A pointer may be null only when its length is 0.
 * Every output is a FyloBytes buffer that FYLO owns; give it back to
 * fylo_bytes_free exactly once. A null output pointer means "ignore it".
 *
 * Results use the JSON of the matching machine-protocol result. Errors are
 * {"code", "message"} with the codes of api/errors/v1.json.
 *
 * A root is a handle number, never a pointer. Calls block and may flush to
 * disk: never call FYLO on the UI thread. Calls may come from any thread.
 *
 * tests/header_matches_code.rs checks this file against src/c_abi.rs.
 */
#ifndef FYLO_H
#define FYLO_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FYLO_ABI_VERSION 1

typedef enum FyloStatus {
    FYLO_OK = 0,
    FYLO_NOT_FOUND = 1,
    FYLO_ERROR = 2,
    FYLO_INVALID_ARGUMENT = 3,
    FYLO_CLOSED = 4,
    FYLO_PANIC = 5
} FyloStatus;

typedef struct FyloBytes {
    uint8_t *ptr;
    size_t len;
} FyloBytes;

/* The ABI this library implements; compare with FYLO_ABI_VERSION. */
uint32_t fylo_abi_version(void);

/*
 * Open the root or .vdbx database at path. options is JSON:
 * {"config": {...}, "identity": {...}, "create": {"mode": ...}}, or empty.
 * key is reserved for an encryption key and must be empty for now.
 */
FyloStatus fylo_open(const uint8_t *path, size_t path_len,
                     const uint8_t *options, size_t options_len,
                     const uint8_t *key, size_t key_len,
                     uint64_t *out_root, FyloBytes *out_error);

/* Close a root. The handle is gone afterwards, even when closing fails. */
FyloStatus fylo_close(uint64_t root, FyloBytes *out_error);

/* Create a document collection. */
FyloStatus fylo_create_collection(uint64_t root,
                                  const uint8_t *name, size_t name_len,
                                  FyloBytes *out);

/* Store a new document; the result is its TTID as a JSON string. */
FyloStatus fylo_put(uint64_t root,
                    const uint8_t *collection, size_t collection_len,
                    const uint8_t *document, size_t document_len,
                    FyloBytes *out);

/* Read one document: {"<id>": {...}}, or FYLO_NOT_FOUND. */
FyloStatus fylo_get(uint64_t root,
                    const uint8_t *collection, size_t collection_len,
                    const uint8_t *id, size_t id_len,
                    FyloBytes *out);

/* Delete one document, keeping its tombstone: {"deleted": true}. */
FyloStatus fylo_delete(uint64_t root,
                       const uint8_t *collection, size_t collection_len,
                       const uint8_t *id, size_t id_len,
                       FyloBytes *out);

/* Every document a structured query matches, in TTID order. */
FyloStatus fylo_find(uint64_t root,
                     const uint8_t *collection, size_t collection_len,
                     const uint8_t *query, size_t query_len,
                     FyloBytes *out);

/*
 * One page of a query with $sort ($limit and $after optional):
 * {"items": [{"id", "doc"}], "nextCursor"}; nextCursor is null on the last
 * page and goes into $after for the next one.
 */
FyloStatus fylo_find_page(uint64_t root,
                          const uint8_t *collection, size_t collection_len,
                          const uint8_t *query, size_t query_len,
                          FyloBytes *out);

/* Make an identity's key where reference names: {"id", "publicKey"}. */
FyloStatus fylo_identity_create(const uint8_t *reference, size_t reference_len,
                                FyloBytes *out);

/* Describe the identity reference names: {"id", "publicKey"}. */
FyloStatus fylo_identity_show(const uint8_t *reference, size_t reference_len,
                              FyloBytes *out);

/* Give back a buffer FYLO returned. An empty buffer is ignored. */
void fylo_bytes_free(FyloBytes bytes);

#ifdef __cplusplus
}
#endif

#endif /* FYLO_H */
