#ifndef SSPI_PROCESS_CONTROL_H
#define SSPI_PROCESS_CONTROL_H
/* Stopping processes from the app (`process-control-v1`), shared by both receivers.
   Only two kinds can be stopped: running apps and games, and payloads started by
   elfldr.elf. System processes, the loader and the receiver itself are refused. */
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define PC_CMD_CONTROL 0x6e
#define PC_NAME_MAX 40

typedef struct {
    int pid, ppid;
    char name[PC_NAME_MAX];
    char title[10];   /* CUSA/PPSA/NPXS..., empty when unknown */
    uint32_t app_id;  /* 0 when the process is not an app */
    uint64_t authid;  /* 0 when unknown (PS4) */
} PcProcess;

typedef enum { PC_NONE = 0, PC_APP = 1, PC_PAYLOAD = 2 } PcKind;

/* What `pid` is, judged against the whole table; PC_NONE with a reason when it may not be stopped. */
PcKind pc_classify(const PcProcess *table, size_t count, int self, int pid, char *reason, size_t cap);
/* "app", "payload" or NULL, for process lists. */
const char *pc_kind_name(PcKind kind);

/* Request: u32le pid, one byte action ('s' stop, 'e' end), then the expected process name,
   NUL-terminated (guards against a reused PID). 0 on success. */
int pc_parse(const uint8_t *body, size_t n, int *pid, char *action, char name[PC_NAME_MAX]);

typedef enum { PC_KILL_APP, PC_SIGTERM, PC_SIGKILL } PcMethod;
/* Stop closes an app through the system (or SIGTERM for a payload); End is SIGKILL. */
PcMethod pc_method(PcKind kind, char action, uint32_t app_id);
const char *pc_method_name(PcMethod method);
#endif
