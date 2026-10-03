/* SSPI integration with GPL-3.0 Payload Manager. */
#ifndef SSPI_SHORTCUT_CONTRACT_H
#define SSPI_SHORTCUT_CONTRACT_H

#include <stddef.h>
#include <string.h>

enum sspi_shortcut_kind { SSPI_SHORTCUT_UNKNOWN, SSPI_SHORTCUT_LAUNCHER,
                          SSPI_SHORTCUT_MANAGER };

/* Recognize only the complete metadata shapes shipped by SSPI/upstream.
 * Reordered, extended or customized metadata is deliberately left alone. */
static inline enum sspi_shortcut_kind sspi_classify_shortcut(const char *json, size_t size) {
    static const char launcher[] = "{\"titleId\":\"WKAL00001\",\"applicationCategoryType\":65536,"
        "\"deeplinkUri\":\"http://127.0.0.1:18181/app/index.html\",\"localizedParameters\":{"
        "\"defaultLanguage\":\"en-US\",\"en-US\":{\"titleName\":\"";
    static const char manager[] = "{\"titleId\":\"PLDM00001\",\"applicationCategoryType\":65536,"
        "\"deeplinkUri\":\"http://127.0.0.1:8084/\",\"localizedParameters\":{"
        "\"defaultLanguage\":\"en-US\",\"en-US\":{\"titleName\":\"";
    static const char ending[] = "\"}}}";
    char compact[4096];
    size_t used = 0;
    int quoted = 0;
    for (size_t i = 0; i < size; i++) {
        unsigned char c = (unsigned char)json[i];
        if (!quoted && (c == ' ' || c == '\t' || c == '\r' || c == '\n')) continue;
        /* No shipped values need escapes; refusing them avoids ambiguous ownership. */
        if (c < 0x20 || c == '\\' || used + 1 >= sizeof(compact)) return SSPI_SHORTCUT_UNKNOWN;
        if (c == '"') quoted = !quoted;
        compact[used++] = (char)c;
    }
    if (quoted) return SSPI_SHORTCUT_UNKNOWN;
    compact[used] = '\0';
    const char *title;
    enum sspi_shortcut_kind kind;
    if (strncmp(compact, launcher, sizeof(launcher) - 1) == 0) {
        title = compact + sizeof(launcher) - 1;
        kind = SSPI_SHORTCUT_LAUNCHER;
    } else if (strncmp(compact, manager, sizeof(manager) - 1) == 0) {
        title = compact + sizeof(manager) - 1;
        kind = SSPI_SHORTCUT_MANAGER;
    } else return SSPI_SHORTCUT_UNKNOWN;
    size_t title_size = strlen(title);
    if (title_size < sizeof(ending) - 1 ||
        strcmp(title + title_size - (sizeof(ending) - 1), ending) != 0)
        return SSPI_SHORTCUT_UNKNOWN;
    compact[used - (sizeof(ending) - 1)] = '\0';
    if (kind == SSPI_SHORTCUT_MANAGER)
        return strcmp(title, "Payload Manager") == 0 || strcmp(title, "SSPI Payload Manager") == 0
            ? kind : SSPI_SHORTCUT_UNKNOWN;
    if (strcmp(title, "SSPI") == 0) return kind;
    /* The earlier SSPI build retained the upstream title plus its edition suffix. */
    static const char legacy[] = "WebKit Autoloader v";
    if (strncmp(title, legacy, sizeof(legacy) - 1) != 0) return SSPI_SHORTCUT_UNKNOWN;
    const char *version = title + sizeof(legacy) - 1;
    const char *edition = strstr(version, "-sspi-");
    if (!edition || edition == version || !edition[6]) return SSPI_SHORTCUT_UNKNOWN;
    for (const char *p = version; p < edition; p++)
        if ((*p < '0' || *p > '9') && *p != '.') return SSPI_SHORTCUT_UNKNOWN;
    for (const char *p = edition + 6; *p; p++)
        if ((*p < '0' || *p > '9') && *p != '.') return SSPI_SHORTCUT_UNKNOWN;
    return kind;
}

static inline int sspi_should_retire_manager_shortcut(enum sspi_shortcut_kind launcher,
                                                      enum sspi_shortcut_kind manager) {
    return launcher == SSPI_SHORTCUT_LAUNCHER && manager == SSPI_SHORTCUT_MANAGER;
}

#endif
