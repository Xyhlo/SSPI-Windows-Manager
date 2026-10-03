#ifndef SSPI_PS5_CONSOLE_TOOLS_H
#define SSPI_PS5_CONSOLE_TOOLS_H
#include "console_files.h"
#define PS5_RECEIVER_VERSION "1.0.14"
#define PS5_LIBRARY_CAP (256u * 1024u)
#define PS5_SYSTEM_INFO_CAP (96u * 1024u)
#define PS5_PROCESSES_CAP (256u * 1024u)
#define PS5_CONSOLE_CAPS "\"pkg-preflight\",\"pkg-install\",\"parallel-upload\",\"verify\",\"extracted-upload\",\"dump-mount\",\"fih-install\",\"title-context\",\"progress-notifications\",\"installed-library-v1\",\"title-icons-v1\",\"system-info-v1\",\"diagnostics-v1\",\"image-publish\",\"process-control-v1\",\"stop\""
bool ps5_title_installed(const char *id);
int ps5_library_json(char *out, size_t cap);
int ps5_metadata(const char *id, uint8_t *out, size_t *size);
int ps5_icon_path(const char *id, char *path, size_t cap);
int ps5_system_info(char *out, size_t cap);
int ps5_processes_json(char *out, size_t cap);
/* Stops an app or payload (process-control-v1); -1 with a sentence in `error` when refused. */
int ps5_process_control(const uint8_t *body, size_t n, char *out, size_t cap, char *error, size_t error_cap);
#endif
