#ifndef SSPI_INSTALL_H
#define SSPI_INSTALL_H
#include "json.h"
typedef struct {
    int user, entitlement; const char *id, *url, *extra, *name, *icon, *sku;
    uint32_t option; const char *scenario, *release, *type, *subtype; uint64_t size;
} GsBgftParam;
typedef struct {
    uint32_t bits; int error; uint64_t length, transferred, length_total, transferred_total;
    uint32_t index, count, seconds, seconds_total; int preparing, copy;
} GsBgftProgress;
_Static_assert(sizeof(GsBgftParam)==104,"BGFT parameter ABI");
_Static_assert(sizeof(GsBgftProgress)==64,"BGFT progress ABI");
typedef struct { char bgft[40], appinst[40], userservice[40]; } ModuleStatus;
int install_init(void);
void install_set_privileges(bool jailbroken, int boot_result, int jbc_result);
void install_modules(ModuleStatus *status);
void install_worker_tick(void);
int install_request(int fd, unsigned command, const uint8_t *body, size_t size);
int install_status_reply(int fd, const char *content_id);
int install_preflight(int fd, bool writable);
int install_inventory_title_ready(const PkgInfo *package);
const char *install_error(int code);
#endif
