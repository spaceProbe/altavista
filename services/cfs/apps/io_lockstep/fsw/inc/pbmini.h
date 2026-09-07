/* A minimal, hand-written protobuf wire-format reader/writer -- just enough to encode/decode
 * the handful of `altavista.v1` messages `proto/altavista/v1/lockstep.proto` defines
 * (the proto directory is read-only for this task; there is no C codegen for it in this
 * repository, so this is a from-scratch implementation of the wire format itself, not of any
 * message descriptor system). Covers exactly what those messages need: varint (int64/uint64/bool),
 * length-delimited (string/bytes/embedded message), and skipping an unrecognized field (proto3
 * forward compatibility -- a field this reader does not know about is skipped, never treated as
 * a parse error, matching every generated protobuf parser's own behaviour).
 *
 * Message-specific encode/decode functions live in lockstep_messages.h/.c, built on these
 * primitives; this header has no knowledge of any particular message shape.
 */
#ifndef AV_CFS_PBMINI_H
#define AV_CFS_PBMINI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define PBMINI_WIRETYPE_VARINT 0u
#define PBMINI_WIRETYPE_FIXED64 1u
#define PBMINI_WIRETYPE_LEN 2u
#define PBMINI_WIRETYPE_FIXED32 5u

typedef struct
{
    uint8_t *buf;
    size_t cap;
    size_t len;
} pbmini_writer_t;

typedef struct
{
    const uint8_t *buf;
    size_t len;
    size_t pos;
} pbmini_reader_t;

typedef enum
{
    PBMINI_OK = 0,
    PBMINI_ERR_BUFFER_TOO_SMALL,
    PBMINI_ERR_TRUNCATED,
    PBMINI_ERR_MALFORMED_VARINT,
    PBMINI_ERR_WRONG_WIRE_TYPE,
} pbmini_status_t;

void pbmini_writer_init(pbmini_writer_t *w, uint8_t *buf, size_t cap);

pbmini_status_t pbmini_write_varint(pbmini_writer_t *w, uint64_t value);
pbmini_status_t pbmini_write_tag(pbmini_writer_t *w, uint32_t field_number, uint32_t wire_type);
pbmini_status_t pbmini_write_varint_field(pbmini_writer_t *w, uint32_t field_number, uint64_t value);
pbmini_status_t pbmini_write_bool_field(pbmini_writer_t *w, uint32_t field_number, bool value);
pbmini_status_t pbmini_write_bytes_field(pbmini_writer_t *w, uint32_t field_number, const uint8_t *data, size_t len);
pbmini_status_t pbmini_write_string_field(pbmini_writer_t *w, uint32_t field_number, const char *s);
/* Writes a length-delimited field whose content is *already encoded* into `submessage`
 * (`len` bytes) -- how every embedded-message field here is built: encode the submessage into
 * a scratch buffer first, then wrap it. */
pbmini_status_t pbmini_write_submessage_field(pbmini_writer_t *w, uint32_t field_number, const uint8_t *submessage, size_t len);

void pbmini_reader_init(pbmini_reader_t *r, const uint8_t *buf, size_t len);
bool pbmini_reader_at_end(const pbmini_reader_t *r);
/* Reads the next field's tag, splitting it into field number and wire type. */
pbmini_status_t pbmini_read_tag(pbmini_reader_t *r, uint32_t *field_number, uint32_t *wire_type);
pbmini_status_t pbmini_read_varint(pbmini_reader_t *r, uint64_t *value);
/* Reads a length-delimited field's length and returns a pointer into the reader's own buffer
 * (no copy) plus its length -- the caller is responsible for bounds-checking any fixed-size
 * destination it copies into. */
pbmini_status_t pbmini_read_len(pbmini_reader_t *r, const uint8_t **data, size_t *len);
/* Skips a field's value given its wire type (used for unrecognized field numbers -- proto3
 * forward compatibility). */
pbmini_status_t pbmini_skip_field(pbmini_reader_t *r, uint32_t wire_type);

#ifdef __cplusplus
}
#endif

#endif /* AV_CFS_PBMINI_H */
