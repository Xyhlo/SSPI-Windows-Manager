/* SSPI integration for itsPLK's GPL-3.0 Payload Manager and unified autoloader. */
#ifndef SSPI_MANAGER_CONTRACT_H
#define SSPI_MANAGER_CONTRACT_H

#include <stddef.h>
#include <stdio.h>
#include <string.h>

#define SSPI_MANAGER_PORT 8084
#define SSPI_MANAGER_ELF_MARKER "SSPI_PLDMGR_EDITION:1"
#define SSPI_MANAGER_IDENTITY_PREFIX \
    "{\"edition\":\"sspi-payload-manager\",\"protocol\":1,\"ready\":true,\"version\":\""
#define SSPI_MANAGER_IDENTITY_JSON \
    SSPI_MANAGER_IDENTITY_PREFIX MENU_VERSION "\"}"

/* Accept only our complete, canonical readiness response, never a port opening. */
static inline int sspi_http_identity_ready(const char *data, size_t size) {
    if (size < 16 || (memcmp(data, "HTTP/1.1 200 ", 13) != 0 &&
                      memcmp(data, "HTTP/1.0 200 ", 13) != 0))
        return 0;
    size_t body = 0;
    for (size_t i = 13; i + 3 < size; i++) {
        if (memcmp(data + i, "\r\n\r\n", 4) == 0) {
            body = i + 4;
            break;
        }
    }
    const size_t prefix = sizeof(SSPI_MANAGER_IDENTITY_PREFIX) - 1;
    if (!body || size - body < prefix + 3 ||
        memcmp(data + body, SSPI_MANAGER_IDENTITY_PREFIX, prefix) != 0 ||
        data[size - 2] != '"' || data[size - 1] != '}')
        return 0;
    for (size_t i = body + prefix; i < size - 2; i++) {
        const unsigned char c = (unsigned char)data[i];
        if (c < 33 || c > 126 || c == '"' || c == '\\') return 0;
    }
    return 1;
}

/* Stream the ELF: a manager can be renamed, and its marker can cross a block. */
static inline int sspi_elf_has_signature(const char *path, const char *signature,
                                         int require_version) {
    const size_t length = strlen(signature);
    if (!length || length > 128) return 0;
    FILE *file = fopen(path, "rb");
    if (!file) return 0;
    unsigned char buffer[8192 + 128];
    if (fread(buffer, 1, 4, file) != 4 || memcmp(buffer, "\177ELF", 4) != 0) {
        fclose(file);
        return 0;
    }
    size_t keep = 0, read_count;
    while ((read_count = fread(buffer + keep, 1, 8192, file)) != 0) {
        const size_t size = keep + read_count;
        for (size_t i = 0; i + length <= size; i++) {
            if (memcmp(buffer + i, signature, length) == 0 &&
                (!require_version || (i + length < size &&
                 buffer[i + length] >= '0' && buffer[i + length] <= '9'))) {
                fclose(file);
                return 1;
            }
        }
        keep = size < length ? size : length;
        memmove(buffer, buffer + size - keep, keep);
    }
    fclose(file);
    return 0;
}

static inline int sspi_is_manager_elf(const char *path) {
    return sspi_elf_has_signature(path, "PLDMGR_VER:", 1);
}

struct sspi_manager_launch {
    int attempted;
    int ready;
};

/* A failed/partial send is never repeated during this autoload invocation. */
static inline int sspi_ensure_manager(struct sspi_manager_launch *state,
                                      int (*probe)(void), int (*send)(void),
                                      void (*pause)(void)) {
    if (state->ready || probe()) {
        state->ready = 1;
        return 0;
    }
    if (state->attempted) return -1;
    state->attempted = 1;
    if (send() != 0) return -1;
    for (int i = 0; i < 50; i++) {
        if (probe()) {
            state->ready = 1;
            return 0;
        }
        pause();
    }
    return -1;
}

#endif
