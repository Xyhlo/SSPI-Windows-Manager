#ifndef SSPI_INSTALLED_LIBRARY_H
#define SSPI_INSTALLED_LIBRARY_H
#include <stddef.h>
#include <stdint.h>

#define INSTALLED_LIBRARY_MAX_TITLES 2048u
#define INSTALLED_LIBRARY_MAX_ERRORS 32u
#define INSTALLED_LIBRARY_MAX_SFO (64u * 1024u)
#define INSTALLED_LIBRARY_MAX_ICON (2u * 1024u * 1024u)
#define INSTALLED_LIBRARY_MAX_METADATA (12u + 2u * INSTALLED_LIBRARY_MAX_SFO + INSTALLED_LIBRARY_MAX_ICON)

typedef int (*InstalledLibraryNameVisitor)(const char *name, void *context);
int installed_library_parse_dirents(const uint8_t *buffer, size_t size, InstalledLibraryNameVisitor visitor, void *context);
int installed_library_list_json(char *out, size_t capacity);
int installed_library_metadata(const char *title_id, uint8_t *out, size_t capacity, size_t *written);
int installed_library_title_present(const char *title_id);

#endif
