#ifndef SSPI_PS5_CONSOLE_TOOLS_H
#define SSPI_PS5_CONSOLE_TOOLS_H
#include "console_files.h"
#define PS5_CONSOLE_CAPS "\"pkg-preflight\",\"pkg-install\",\"parallel-upload\",\"verify\",\"extracted-upload\",\"dump-mount\",\"fih-install\",\"title-context\",\"progress-notifications\",\"installed-library-v1\",\"title-icons-v1\",\"system-info-v1\",\"stop\""
bool ps5_title_installed(const char *id);
int ps5_library_json(char *out, size_t cap);
int ps5_metadata(const char *id, uint8_t *out, size_t *size);
int ps5_system_info(char *out, size_t cap);
#endif
