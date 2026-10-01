#ifndef SSPI_DIAGNOSTICS_H
#define SSPI_DIAGNOSTICS_H
/* Read-only console diagnostics shared by the PS5 and PS4 receivers: the kernel
   log, log/config/crash files left by other payloads, and their tails. */
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define DX_CMD_KERNEL_LOG 0x69
#define DX_CMD_PROCESSES 0x6a
#define DX_CMD_LOG_LIST 0x6b
#define DX_CMD_LOG_READ 0x6c
#define DX_TEXT_MAX (1024u * 1024u)
#define DX_REPLY_MAX (DX_TEXT_MAX + 4096u)
#define DX_LIST_MAX (192u * 1024u)
#define DX_LOG_FILES 512u

/* Platform hooks: each receiver and the host tests provide these. */
typedef int (*DxVisit)(const char *name, void *context); /* nonzero stops the listing */
int dx_platform_list(const char *dir, DxVisit visit, void *context); /* -1 when unreadable */
/* 0 absent, 1 regular file, 2 directory, -1 link, device or unreadable. Never follows links. */
int dx_platform_lstat(const char *path, uint64_t *size, int64_t *modified);
/* Both log hooks run under the diagnostics lock and return the failing errno in
   `error` (zero on success), before cleanup can overwrite the native errno. */
/* The newest bytes of the kernel message buffer (sysctl kern.msgbuf); -1 when unavailable. */
int dx_platform_msgbuf(char *out, size_t cap, size_t *size, int *error);
/* Drains what /dev/klog has queued without blocking or keeping it open:
   0 read (possibly nothing), -2 another klog server holds it, -1 unavailable. */
int dx_platform_klog_drain(char *out, size_t cap, size_t *size, int *error);
void dx_platform_lock(void);
void dx_platform_unlock(void);
uint64_t dx_platform_ms(void);

/* Each builds a complete RESP_DATA body, or returns -1 with a sentence in `error`. */
int dx_kernel_log(char *out, size_t cap, size_t *size, char *error, size_t error_cap);
int dx_log_list(char *out, size_t cap, size_t *size, char *error, size_t error_cap);
/* Body: u32le byte limit, then a NUL-terminated absolute path. Replies with the file's tail. */
int dx_log_read(const uint8_t *body, size_t n, char *out, size_t cap, size_t *size, char *error, size_t error_cap);

/* Quotes at most `max` bytes of `text` (fixed kernel arrays need not be NUL-terminated)
   as a JSON string; non-ASCII and control bytes become '?' so the reply stays valid UTF-8. */
void dx_json_string(char *out, size_t cap, const char *text, size_t max);
/* 0 not listed, 1 log, 2 configuration, 3 crash report. */
int dx_file_kind(const char *name);
bool dx_readable_path(const char *path);
void dx_klog_reset(void);
#endif
