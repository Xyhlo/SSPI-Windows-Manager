#define _GNU_SOURCE
#include "installed_library.h"
#include "install.h"
#include "json.h"
#include "pkg.h"
#include "platform.h"
#include "proto.h"
#include "../payload/console_files.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#if !defined(SSPI_HOST_TEST) && !defined(_WIN32)
#include <unistd.h>
extern int sceKernelGetdents(int fd, void *buffer, size_t size);
#else
int installed_library_host_open(const char *path);
int installed_library_host_close(int fd);
int installed_library_host_getdents(int fd, void *buffer, size_t size);
#endif

#define DIRECTORY_BUFFER_SIZE 8192u
#define PKG_HEADER_SIZE 4096u
#define PKG_ENTRY_SIZE 32u
#define MAX_PKG_ENTRIES 65536u
#define ERROR_TEXT_SIZE 96u

typedef struct {
    char ids[INSTALLED_LIBRARY_MAX_TITLES][10];
    size_t count;
    bool truncated;
    bool errors_truncated;
    char errors[INSTALLED_LIBRARY_MAX_ERRORS][ERROR_TEXT_SIZE];
    size_t error_count;
} LibraryScan;

static uint32_t read_be32(const uint8_t *p) {
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3];
}

static void add_error(LibraryScan *scan, const char *title_id, const char *reason) {
    if (scan->error_count >= INSTALLED_LIBRARY_MAX_ERRORS) { scan->errors_truncated = true; return; }
    char *error = scan->errors[scan->error_count++];
    if (title_id && title_id[0]) snprintf(error, ERROR_TEXT_SIZE, "%s: %s", title_id, reason);
    else snprintf(error, ERROR_TEXT_SIZE, "%s", reason);
}

static bool already_listed(const LibraryScan *scan, const char *id) {
    for (size_t i = 0; i < scan->count; ++i) if (!strcmp(scan->ids[i], id)) return true;
    return false;
}

static int package_at(const char *path, const char *title_id, PkgInfo *package) {
    int fd = rx_open(path, RX_READ);
    if (fd < 0) return -1;
    int rc = pkg_read(fd, package);
    if (rx_close(fd)) rc = -1;
    if (rc || package->kind != PKG_BASE || strcmp(package->title_id, title_id)) return -1;
    return 0;
}

static int find_base_package(const char *title_id, PkgInfo *package) {
    const char *roots[] = {"", "/mnt/ext0"};
    for (size_t i = 0; i < sizeof(roots) / sizeof(roots[0]); ++i) {
        char path[192];
        int n = snprintf(path, sizeof(path), "%s/user/app/%s/app.pkg", roots[i], title_id);
        if (n < 0 || (size_t)n >= sizeof(path)) return -1;
        if (!package_at(path, title_id, package)) return 0;
    }
    return -1;
}
int installed_library_title_present(const char *title_id) {
    PkgInfo package;
    return ct_valid_id(title_id) && !memcmp(title_id,"CUSA",4) && !find_base_package(title_id,&package);
}

static int remember_candidate(const char *name, void *context) {
    LibraryScan *scan = context;
    if (!valid_title_id(name) || memcmp(name, "CUSA", 4)) return 0;
    if (already_listed(scan, name)) return 0;
    if (scan->count >= INSTALLED_LIBRARY_MAX_TITLES) { scan->truncated = true; return 1; }
    PkgInfo package;
    if (find_base_package(name, &package)) {
        add_error(scan, name, "installed app.pkg is missing or invalid");
        return 0;
    }
    if (!install_inventory_title_ready(&package)) {
        add_error(scan, name, "PS4 install is active or not confirmed");
        return 0;
    }
    memcpy(scan->ids[scan->count++], name, 10);
    return 0;
}

int installed_library_parse_dirents(const uint8_t *buffer, size_t size, InstalledLibraryNameVisitor visitor, void *context) {
    if ((!buffer && size) || !visitor) return -1;
    size_t offset = 0;
    while (offset < size) {
        if (size - offset < 9) return -1;
        const uint8_t *entry = buffer + offset;
        uint16_t record_length = (uint16_t)entry[4] | (uint16_t)((uint16_t)entry[5] << 8);
        size_t name_length = entry[7];
        if (record_length < 9 || record_length > size - offset || name_length > record_length - 9) return -1;
        if (entry[8 + name_length] != 0) return -1;
        char name[256];
        memcpy(name, entry + 8, name_length);
        name[name_length] = 0;
        int rc = visitor(name, context);
        if (rc) return rc;
        offset += record_length;
    }
    return 0;
}

static int directory_open(const char *path) {
#if defined(SSPI_HOST_TEST) || defined(_WIN32)
    return installed_library_host_open(path);
#else
    return open(path, O_RDONLY | O_DIRECTORY);
#endif
}

static int directory_close(int fd) {
#if defined(SSPI_HOST_TEST) || defined(_WIN32)
    return installed_library_host_close(fd);
#else
    return close(fd);
#endif
}

static int directory_read(int fd, void *buffer, size_t capacity) {
#if defined(SSPI_HOST_TEST) || defined(_WIN32)
    return installed_library_host_getdents(fd, buffer, capacity);
#else
    return sceKernelGetdents(fd, buffer, capacity);
#endif
}

static int scan_directory(LibraryScan *scan, const char *path, bool optional) {
    int fd = directory_open(path);
    if (fd < 0) {
        if (optional) {
#ifndef SSPI_HOST_TEST
            struct stat st;
            if (stat(path, &st) && errno == ENOENT) return 0;
#endif
        }
        add_error(scan, NULL, "could not enumerate the PS4 installed-app directory");
        return -1;
    }
    uint8_t buffer[DIRECTORY_BUFFER_SIZE];
    int result = 0;
    for (;;) {
        int n = directory_read(fd, buffer, sizeof(buffer));
        if (n < 0 || (size_t)n > sizeof(buffer)) {
            add_error(scan, NULL, "PS4 installed-app directory read failed");
            result = -1;
            break;
        }
        if (!n) break;
        int parsed = installed_library_parse_dirents(buffer, (size_t)n, remember_candidate, scan);
        if (parsed < 0) {
            add_error(scan, NULL, "PS4 returned malformed directory entries");
            result = -1;
            break;
        }
        if (parsed > 0) break;
    }
    if (directory_close(fd)) {
        add_error(scan, NULL, "could not close the PS4 installed-app directory");
        result = -1;
    }
    return result;
}

int installed_library_list_json(char *out, size_t capacity) {
    if (!out || !capacity) return -1;
    LibraryScan scan = {0};
    scan_directory(&scan, "/user/app", false);
    if (!scan.truncated) scan_directory(&scan, "/mnt/ext0/user/app", true);

    Json json;
    json_init(&json, out, capacity);
    json_add(&json, "{\"titles\":[");
    for (size_t i = 0; i < scan.count; ++i) {
        if (i) json_add(&json, ",");
        json_quote(&json, scan.ids[i]);
    }
    json_add(&json, "],\"complete\":%s,\"truncated\":%s,\"errorsTruncated\":%s,\"errors\":[",
        (!scan.truncated && !scan.error_count) ? "true" : "false", scan.truncated ? "true" : "false", scan.errors_truncated ? "true" : "false");
    for (size_t i = 0; i < scan.error_count; ++i) {
        if (i) json_add(&json, ",");
        json_quote(&json, scan.errors[i]);
    }
    json_add(&json, "],\"customIcons\":[");
    unsigned custom=0;
    for (size_t i=0;i<scan.count;i++) if (ct_custom_icon(DATA_ROOT,scan.ids[i])) {
        if (custom++) json_add(&json,",");
        json_quote(&json,scan.ids[i]);
    }
    json_add(&json, "]}");
    return json.failed ? -1 : 0;
}

static int read_optional_file(const char *path, uint8_t *out, size_t limit, size_t *length) {
    int fd = rx_open(path, RX_READ);
    if (fd < 0) return -1;
    uint64_t size = 0;
    int rc = rx_size(fd, &size);
    if (rc || !size || size > limit) rc = -1;
    else if (rx_read_exact(fd, out, (size_t)size, 0)) rc = -1;
    if (rx_close(fd)) rc = -1;
    if (!rc) *length = (size_t)size;
    return rc;
}

static int read_package_entry(const char *path, const char *title_id, uint32_t kind, uint32_t entry_id,
    uint8_t *out, size_t capacity, size_t *length) {
    int fd = rx_open(path, RX_READ);
    if (fd < 0) return -1;
    PkgInfo package;
    uint64_t file_size = 0;
    int rc = pkg_read(fd, &package) || rx_size(fd, &file_size) || strcmp(package.title_id, title_id) || package.kind != (enum PkgKind)kind ? -1 : 0;
    uint8_t header[PKG_HEADER_SIZE], raw[PKG_ENTRY_SIZE];
    if (!rc && rx_read_exact(fd, header, sizeof(header), 0)) rc = -1;
    uint32_t count = rc ? 0 : read_be32(header + 0x10);
    uint64_t table_offset = rc ? 0 : read_be32(header + 0x18);
    if (!rc && (count > MAX_PKG_ENTRIES || table_offset > file_size || (uint64_t)count * PKG_ENTRY_SIZE > file_size - table_offset)) rc = -1;
    bool found = false;
    for (uint32_t i = 0; !rc && i < count; ++i) {
        if (rx_read_exact(fd, raw, sizeof(raw), table_offset + (uint64_t)i * PKG_ENTRY_SIZE)) { rc = -1; break; }
        if (read_be32(raw) != entry_id || (read_be32(raw + 8) & 0x80000000u)) continue;
        uint64_t offset = read_be32(raw + 0x10), size = read_be32(raw + 0x14);
        if (!size || size > capacity || offset > file_size || size > file_size - offset) { rc = -1; break; }
        if (rx_read_exact(fd, out, (size_t)size, offset)) rc = -1;
        else { *length = (size_t)size; found = true; }
        break;
    }
    if (rx_close(fd)) rc = -1;
    return rc || !found ? -1 : 0;
}

static int read_title_metadata(const char *title_id, uint8_t *out, size_t capacity, size_t *length) {
    const char *roots[] = {"", "/mnt/ext0"};
    for (size_t i = 0; i < sizeof(roots) / sizeof(roots[0]); ++i) {
        char path[192];
        int n = snprintf(path, sizeof(path), "%s/user/app/%s/app.pkg", roots[i], title_id);
        if (n < 0 || (size_t)n >= sizeof(path)) continue;
        if (!read_package_entry(path, title_id, PKG_BASE, 0x1000, out, capacity, length)) return 0;
    }
    char path[192];
    const char *metadata_roots[] = {"/user/appmeta", "/system_data/priv/appmeta"};
    for (size_t i = 0; i < sizeof(metadata_roots) / sizeof(metadata_roots[0]); ++i) {
        int n = snprintf(path, sizeof(path), "%s/%s/param.sfo", metadata_roots[i], title_id);
        if (n >= 0 && (size_t)n < sizeof(path) && !read_optional_file(path, out, capacity, length)) return 0;
    }
    return -1;
}

static int read_patch_metadata(const char *title_id, uint8_t *out, size_t capacity, size_t *length) {
    const char *roots[] = {"", "/mnt/ext0"};
    for (size_t i = 0; i < sizeof(roots) / sizeof(roots[0]); ++i) {
        char path[192];
        int n = snprintf(path, sizeof(path), "%s/user/patch/%s/patch.pkg", roots[i], title_id);
        if (n >= 0 && (size_t)n < sizeof(path) && !read_package_entry(path, title_id, PKG_UPDATE, 0x1000, out, capacity, length)) return 0;
    }
    return -1;
}

static int read_icon(const char *title_id, uint8_t *out, size_t capacity, size_t *length) {
    /* The library reflects a custom home icon, while preserving the legacy 1 MiB limit. */
    for (size_t i=0;i<ct_metadata_root_count;i++) {
        char path[192]; snprintf(path,sizeof(path),"%s/%s/icon0.png",ct_metadata_roots[i],title_id);
        if (!ct_read(path,out,capacity,length) && ct_valid_png(out,*length)) return 0;
    }
    const char *roots[] = {"", "/mnt/ext0"};
    for (size_t i = 0; i < sizeof(roots) / sizeof(roots[0]); ++i) {
        char path[192];
        int n = snprintf(path, sizeof(path), "%s/user/app/%s/app.pkg", roots[i], title_id);
        if (n >= 0 && (size_t)n < sizeof(path) && !read_package_entry(path, title_id, PKG_BASE, 0x1200, out, capacity, length)) return 0;
    }
    const char *metadata_roots[] = {"/user/appmeta", "/system_data/priv/appmeta"};
    for (size_t i = 0; i < sizeof(metadata_roots) / sizeof(metadata_roots[0]); ++i) {
        char path[192];
        int n = snprintf(path, sizeof(path), "%s/%s/icon0.png", metadata_roots[i], title_id);
        if (n >= 0 && (size_t)n < sizeof(path) && !read_optional_file(path, out, capacity, length)) return 0;
    }
    return -1;
}

int installed_library_metadata(const char *title_id, uint8_t *out, size_t capacity, size_t *written) {
    if (!valid_title_id(title_id) || memcmp(title_id, "CUSA", 4) || !out || !written) return -1;
    if (capacity < INSTALLED_LIBRARY_MAX_METADATA) return -1;
    PkgInfo base_package;
    if (find_base_package(title_id, &base_package)) return -1;
    size_t offset = 0, base_length = 0, patch_length = 0, icon_length = 0;
    uint8_t *base_length_field = out + offset; offset += 4;
    (void)read_title_metadata(title_id, out + offset, INSTALLED_LIBRARY_MAX_SFO, &base_length);
    write_u32le(base_length_field, (uint32_t)base_length); offset += base_length;
    uint8_t *patch_length_field = out + offset; offset += 4;
    if (read_patch_metadata(title_id, out + offset, INSTALLED_LIBRARY_MAX_SFO, &patch_length)) patch_length = 0;
    write_u32le(patch_length_field, (uint32_t)patch_length); offset += patch_length;
    uint8_t *icon_length_field = out + offset; offset += 4;
    if (read_icon(title_id, out + offset, INSTALLED_LIBRARY_MAX_ICON, &icon_length)) icon_length = 0;
    write_u32le(icon_length_field, (uint32_t)icon_length); offset += icon_length;
    *written = offset;
    return 0;
}

/* System themes are installed per content ID under INSTALLED_THEME_ROOT. */
typedef struct { char ids[INSTALLED_LIBRARY_MAX_THEMES][37]; size_t count; bool truncated; } ThemeScan;
static int remember_theme(const char *name, void *context) {
    ThemeScan *scan = context;
    if (!valid_content_id(name)) return 0;
    if (scan->count >= INSTALLED_LIBRARY_MAX_THEMES) { scan->truncated = true; return 1; }
    snprintf(scan->ids[scan->count++], sizeof(scan->ids[0]), "%s", name);
    return 0;
}
void installed_library_sfo_string(const uint8_t *sfo, size_t size, const char *key, char *out, size_t capacity) {
    if (!out || !capacity) return;
    out[0] = 0;
    if (!sfo || size < 20 || memcmp(sfo, "\0PSF", 4)) return;
    uint32_t keys = read_u32le(sfo + 8), data = read_u32le(sfo + 12), count = read_u32le(sfo + 16);
    if (keys > size || data > size || count > 1024 || 20u + (size_t)count * 16u > size) return;
    size_t wanted = strlen(key);
    for (uint32_t i = 0; i < count; ++i) {
        const uint8_t *entry = sfo + 20 + (size_t)i * 16;
        size_t name = keys + ((size_t)entry[0] | ((size_t)entry[1] << 8));
        uint16_t format = (uint16_t)(entry[2] | (entry[3] << 8));
        uint32_t length = read_u32le(entry + 4), offset = read_u32le(entry + 12);
        if (name + wanted + 1 > size || memcmp(sfo + name, key, wanted + 1)) continue;
        if (format != 0x0204 || (size_t)data + offset > size || length > size - data - offset) return;
        size_t n = length && sfo[data + offset + length - 1] == 0 ? length - 1 : length;
        if (n >= capacity) n = capacity - 1;
        memcpy(out, sfo + data + offset, n);
        out[n] = 0;
        for (size_t c = 0; c < n; ++c) if ((unsigned char)out[c] < 0x20) out[c] = ' ';
        return;
    }
}
static int scan_themes(ThemeScan *scan) {
    int fd = directory_open(INSTALLED_THEME_ROOT);
    if (fd < 0) return 0;  /* no theme has been installed yet */
    uint8_t buffer[DIRECTORY_BUFFER_SIZE];
    int result = 0;
    for (;;) {
        int n = directory_read(fd, buffer, sizeof(buffer));
        if (n < 0 || (size_t)n > sizeof(buffer)) { result = -1; break; }
        if (!n) break;
        int parsed = installed_library_parse_dirents(buffer, (size_t)n, remember_theme, scan);
        if (parsed < 0) { result = -1; break; }
        if (parsed > 0) break;
    }
    if (directory_close(fd)) result = -1;
    return result;
}
int installed_library_theme_present(const char *content_id) {
    if (!valid_content_id(content_id)) return 0;
    char path[160];
    int n = snprintf(path, sizeof(path), "%s/%s/ac.pkg", INSTALLED_THEME_ROOT, content_id);
    if (n < 0 || (size_t)n >= sizeof(path)) return 0;
    int fd = rx_open(path, RX_READ);
    if (fd < 0) return 0;
    PkgInfo package;
    int ok = !pkg_read(fd, &package) && package.iro_tag == 2 && !strcmp(package.content_id, content_id);
    if (rx_close(fd)) ok = 0;
    return ok;
}
int installed_library_themes_json(char *out, size_t capacity, const char *active) {
    if (!out || !capacity) return -1;
    static ThemeScan scan;
    static uint8_t sfo[INSTALLED_LIBRARY_MAX_SFO];
    memset(&scan, 0, sizeof(scan));
    if (scan_themes(&scan)) return -1;
    Json json;
    json_init(&json, out, capacity);
    json_add(&json, "{\"themes\":[");
    for (size_t i = 0; i < scan.count; ++i) {
        char path[160], title[130] = "", title_id[10];
        snprintf(path, sizeof(path), "%s/%s/ac.pkg", INSTALLED_THEME_ROOT, scan.ids[i]);
        memcpy(title_id, scan.ids[i] + 7, 9); title_id[9] = 0;
        size_t length = 0;
        if (!read_package_entry(path, title_id, PKG_DLC, 0x1000, sfo, sizeof(sfo), &length))
            installed_library_sfo_string(sfo, length, "TITLE", title, sizeof(title));
        if (i) json_add(&json, ",");
        json_add(&json, "{\"contentId\":");
        json_quote(&json, scan.ids[i]);
        json_add(&json, ",\"title\":");
        json_quote(&json, title);
        json_add(&json, "}");
    }
    json_add(&json, "],\"truncated\":%s,\"active\":", scan.truncated ? "true" : "false");
    json_quote(&json, active ? active : "");
    json_add(&json, "}");
    return json.failed ? -1 : 0;
}
