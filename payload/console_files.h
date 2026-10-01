#ifndef SSPI_CONSOLE_FILES_H
#define SSPI_CONSOLE_FILES_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#define CT_MAX_PNG (2u * 1024u * 1024u)
#define CT_MAX_META (64u * 1024u)
#define CT_METADATA_SIZE (12u + 2u * CT_MAX_META + CT_MAX_PNG)
extern const char *const ct_metadata_roots[];
extern const size_t ct_metadata_root_count;
bool ct_valid_id(const char *id);
bool ct_valid_png(const uint8_t *bytes, size_t size);
/* 0 absent, 1 regular file, 2 directory, -1 unsafe/unreadable. */
int ct_kind(const char *path);
int ct_read(const char *path, uint8_t *bytes, size_t capacity, size_t *size);
int ct_read_at(const char *path, uint8_t *bytes, size_t size, uint64_t offset);
/* 0 committed, -1 not renamed, 1 renamed but parent-directory sync failed. */
int ct_atomic_write(const char *path, const uint8_t *bytes, size_t size);
bool ct_custom_icon(const char *data_root, const char *id);
int ct_icon_get(const char *data_root, const char *id, bool original, uint8_t *out, size_t *size);
int ct_icon_change(const char *data_root, const char *platform, const char *id,
    const uint8_t *png, size_t size, bool restore, char *result, size_t capacity);
/* PS4 only: every icon copy of a title (icon0.png, the icon0.dds the home screen draws and the
   per-language icon0_NN copies). `dds` may be NULL to leave the DDS copies alone. */
#define CT_MAX_DDS (1024u * 1024u)
bool ct_valid_dds(const uint8_t *bytes, size_t size);
int ct_ps4_icon_change(const char *data_root, const char *id, const uint8_t *png, size_t png_size,
    const uint8_t *dds, size_t dds_size, bool restore, char *result, size_t capacity);
void ct_json_quote(char *out, size_t capacity, const char *text);
#endif
