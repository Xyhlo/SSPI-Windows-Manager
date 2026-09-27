#ifndef SSPI_JSON_H
#define SSPI_JSON_H
#include "pkg.h"
typedef struct { char *data; size_t cap, used; bool failed; } Json;
void json_init(Json *j, char *data, size_t cap);
void json_add(Json *j, const char *format, ...);
void json_quote(Json *j, const char *s);
typedef struct {
    char url[2048], content_id[37], title[220], title_id[10], icon_url[1000];
    enum PkgKind kind; uint64_t size, declared_size; uint32_t content_type;
    bool has_declared_size, has_content_type, theme;
    char digest[65], header_sha256[65];
} UrlRequest;
int json_object_valid(const char *data, size_t size);
int parse_install_url(const uint8_t *body, size_t size, UrlRequest *out, char *error, size_t cap);
typedef struct {
    int api_code, error_code, progress;
    const char *content_id, *state, *status, *error;
    uint64_t downloaded, total;
} InstallStatus;
int submission_json(char *out, size_t cap, int code, const char *cid, const char *path, const char *error, int task, bool url);
int status_json(char *out, size_t cap, const InstallStatus *status);
int preflight_error_json(char *out, size_t cap, int code, const char *stage, const char *error);
int privilege_error_json(char *out, size_t cap, bool jailbroken, int boot_result, int jbc_result);
typedef struct {
    int port, uid; bool jailbroken, writable;
    const char *bgft, *appinst, *userservice;
} ReceiverConfig;
int config_json(char *out, size_t cap, const ReceiverConfig *config);
#endif
