#ifndef SSPI_PKG_H
#define SSPI_PKG_H
#include "platform.h"
enum PkgKind { PKG_OTHER, PKG_BASE=6, PKG_DLC=7, PKG_UPDATE=8 };
typedef struct {
    char content_id[37], title_id[10];
    enum PkgKind kind;
    uint32_t content_type, iro_tag;
    uint64_t size;
    uint8_t header[4096];
} PkgInfo;
bool valid_title_id(const char *s);
bool valid_content_id(const char *s);
enum PkgKind pkg_kind(const char *name);
const char *pkg_bgft_type(uint32_t content_type);
int pkg_parse(const uint8_t *header, size_t length, uint64_t size, PkgInfo *info);
int pkg_read(int fd, PkgInfo *info);
int pkg_magic(int fd, uint64_t size);
int pkg_files_equal(int left, int right, uint64_t size);
int pkg_verify_installed_copy(int source, const PkgInfo *expected, char *path, size_t cap);
int pkg_installed_path(const PkgInfo *info, char *path, size_t size);
int pkg_header_hash(int fd, char hash[65]);
int pkg_find_installed(const PkgInfo *expected, const char *header_sha256, const char *digest,
    PkgInfo *actual, char *path, size_t cap, uint64_t *stamp);
#endif
