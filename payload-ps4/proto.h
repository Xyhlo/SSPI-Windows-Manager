#ifndef SSPI_PROTO_H
#define SSPI_PROTO_H
#include "platform.h"
#define VERSION "1.0.5"
#define DEFAULT_PORT 9114
#define MAX_FRAME (8u * 1024u * 1024u)
#define MAX_PATH_BYTES 2048
#define UPLOAD_BUFFER (256u * 1024u)
#define MAX_CLIENTS 48u
#define MAX_UPLOAD_LANES 32u
#define MAX_TRANSFERS 64u
#define MAX_JOBS 32u
enum {
    CMD_PING=0x01, CMD_CREATE_DIR=0x04, CMD_START_UPLOAD=0x10,
    CMD_UPLOAD_CHUNK=0x11, CMD_END_UPLOAD=0x12, CMD_INSTALL_PKG=0x50,
    CMD_INSTALL_STATUS=0x51, CMD_GET_CONFIG=0x53, CMD_SET_PORT=0x54,
    CMD_VERIFY_FILE=0x55, CMD_INSTALL_PREFLIGHT=0x56, CMD_TITLE_CONTEXT=0x57,
    CMD_PROGRESS_NOTIFICATION=0x58, CMD_INSTALL_URL=0x59, CMD_STOP=0x5a,
    CMD_CANCEL_INSTALL=0x5b, CMD_PAUSE_INSTALL=0x5c, CMD_RESUME_INSTALL=0x5d,
    CMD_LIST_INSTALLED=0x5e, CMD_INSTALLED_METADATA=0x5f,
    CMD_TITLE_ICON_GET=0x60, CMD_TITLE_ICON_SET=0x61, CMD_TITLE_ICON_RESTORE=0x62,
    CMD_SHELL_REFRESH=0x63, CMD_SYSTEM_INFO=0x64, CMD_INSTALL_THEME=0x65,
    CMD_THEME_LIST=0x66, CMD_THEME_APPLY=0x67, CMD_THEME_DELETE=0x68,
    RESP_OK=1, RESP_ERROR=2, RESP_DATA=3, RESP_READY=4
};
static inline uint32_t read_u32le(const uint8_t *p) { return (uint32_t)p[0] | ((uint32_t)p[1]<<8) | ((uint32_t)p[2]<<16) | ((uint32_t)p[3]<<24); }
static inline uint64_t read_u64le(const uint8_t *p) { return read_u32le(p) | ((uint64_t)read_u32le(p+4)<<32); }
static inline void write_u32le(uint8_t *p, uint32_t v) { for (unsigned i=0;i<4;i++) p[i]=(uint8_t)(v>>(8*i)); }
int send_all(int fd, const void *data, size_t size);
#define RECEIVE_WINDOW_MS 30000u
#define RECEIVE_MIN_BYTES (64u*1024u)
typedef struct { uint64_t started; size_t received; bool streaming; } ReceiveDeadline;
void receive_begin(ReceiveDeadline *deadline, size_t frame_size);
int recv_all_deadline(int fd, void *data, size_t size, ReceiveDeadline *deadline);
int recv_all(int fd, void *data, size_t size);
int read_frame_deadline(int fd, uint8_t *cmd, uint32_t *size, ReceiveDeadline *deadline);
int read_frame(int fd, uint8_t *cmd, uint32_t *size);
int reply(int fd, uint8_t code, const void *data, uint32_t size);
int text_reply(int fd, uint8_t code, const char *text);
const char *wire_string(const uint8_t *body, size_t size, size_t limit);
int parse_port(const char *text, size_t size, bool config);
#endif
