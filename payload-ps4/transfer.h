#ifndef SSPI_TRANSFER_H
#define SSPI_TRANSFER_H
#include "proto.h"
#include "pkg.h"
typedef struct Segment Segment;
typedef struct Transfer Transfer;
typedef struct { Transfer *transfer; Segment *segment; uint64_t offset, expected, received; } Lane;
bool allowed_path(const char *path);
int handle_create_dir(int fd, const char *path);
int handle_start(int fd, const uint8_t *body, uint32_t size, Lane *lane);
int handle_upload_chunk(int fd, uint32_t size, Lane *lane, uint8_t *buffer);
int handle_upload_chunk_deadline(int fd, uint32_t size, Lane *lane, uint8_t *buffer, ReceiveDeadline *deadline);
int handle_end(int fd, Lane *lane);
int handle_verify(int fd, const char *path);
void release_lane(Lane *lane);
int transfer_pin(const char *path, PkgInfo *info);
void transfer_finish(const char *path, bool installed);
void transfer_reset(void);
#endif
