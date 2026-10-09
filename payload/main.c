#include <arpa/inet.h>
#include <errno.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <pthread.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <ctype.h>
#include <dirent.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/mount.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <unistd.h>
#include <time.h>
#include <ps5/kernel.h>
#include "console_tools.h"
#include "diagnostics.h"
#include "process_control.h"
#include "install_process.h"
#ifndef MNT_UPDATE
#define MNT_UPDATE 0x00000010
#endif
#ifndef PATH_MAX
#define PATH_MAX 1024
#endif
#define IOVEC_ENTRY(x) { (void *)(x), (x) ? strlen(x) + 1 : 0 }
#define IOVEC_SIZE(x) (sizeof(x) / sizeof(struct iovec))

#define VERSION PS5_RECEIVER_VERSION
extern void sspi_startup_note(const char *stage, int result);
#define PS5_INSTALL_CAPS PS5_CONSOLE_CAPS ",\"install-reconcile-v1\""
#define DEFAULT_PORT 9114
#define MAX_FRAME (8u * 1024u * 1024u)
#define MAX_PATH_BYTES 2048
#define UPLOAD_BUFFER (256u * 1024u)
#define MAX_CLIENTS 48u
#define MAX_UPLOAD_LANES 32u
#define MAX_TRANSFERS 64u
#define CLIENT_STACK (256u * 1024u)
#define CMD_PING 0x01
#define CMD_CREATE_DIR 0x04
#define CMD_START_UPLOAD 0x10
#define CMD_UPLOAD_CHUNK 0x11
#define CMD_END_UPLOAD 0x12
#define CMD_INSTALL_PKG 0x50
#define CMD_INSTALL_STATUS 0x51
#define CMD_MOUNT_GAME 0x52
#define CMD_GET_CONFIG 0x53
#define CMD_SET_PORT 0x54
#define CMD_VERIFY_FILE 0x55
#define CMD_INSTALL_PREFLIGHT 0x56
#define CMD_TITLE_CONTEXT 0x57
#define CMD_PROGRESS_NOTIFICATION 0x58
#define CMD_CONFIRM_INSTALL 0x59
#define CMD_STOP 0x5a
#define CMD_LIST_INSTALLED 0x5e
#define CMD_INSTALLED_METADATA 0x5f
#define CMD_TITLE_ICON_GET 0x60
#define CMD_TITLE_ICON_SET 0x61
#define CMD_TITLE_ICON_RESTORE 0x62
#define CMD_SHELL_REFRESH 0x63
#define CMD_SYSTEM_INFO 0x64
#define CMD_IMAGE 0x6d
#define RESP_OK 0x01
#define RESP_ERROR 0x02
#define RESP_DATA 0x03
#define RESP_READY 0x04
#define SYSTEM_INSTALL_AUTHID UINT64_C(0x4801000000000013)
#ifndef O_NOFOLLOW
#define O_NOFOLLOW 0
#endif

/* PS5 notification ABI: message +0x2d, icon URI +0x42d, total 0xc30. */
typedef struct {
    int32_t type, request_id, priority, message_id, target_id, user_id;
    int32_t unknown1, unknown2, app_id, error_number, unknown3;
    char use_icon_uri;
    char message[1024], uri[1024], reserved[1024];
} Notification;
_Static_assert(sizeof(Notification) == 0xc30, "notification ABI size");
_Static_assert(offsetof(Notification, uri) == 0x42d, "notification icon offset");
extern int sceKernelSendNotificationRequest(int, Notification *, size_t, int);
static int sceNotificationSend(int user_id, bool is_logged, const char *payload);

/* Layouts and entry points follow the public etaHEN DirectPKGInstaller ABI. */
typedef char content_id_t[0x30];
typedef struct { content_id_t content_id; int content_type; int content_platform; } SceAppInstallPkgInfo;
typedef struct { const char *uri; const char *ex_uri; const char *playgo_scenario_id; const char *content_id; const char *content_name; const char *icon_url; } MetaInfo;
typedef struct { char languages[30][8]; char playgo_scenario_ids[64][3]; char content_ids[64][0x30]; int64_t unknown[810]; } PlayGoInfo;
typedef struct { int32_t error_code; int32_t version; char description[512]; char type[9]; } SceAppInstallErrorInfo;
typedef struct { char status[16]; char src_type[8]; uint32_t remain_time; uint64_t downloaded_size; uint64_t initial_chunk_size; uint64_t total_size; uint32_t promote_progress; SceAppInstallErrorInfo error_info; int32_t local_copy_percent; bool is_copy_only; } SceAppInstallStatusInstalled;
_Static_assert(sizeof(SceAppInstallPkgInfo) == 0x38 && offsetof(SceAppInstallPkgInfo, content_type) == 0x30, "package info ABI");
_Static_assert(sizeof(MetaInfo) == 0x30 && offsetof(MetaInfo, icon_url) == 0x28, "install metadata ABI");
_Static_assert(sizeof(PlayGoInfo) == 0x2700 && offsetof(PlayGoInfo, unknown) == 0xdb0, "PlayGo ABI");
_Static_assert(sizeof(SceAppInstallStatusInstalled) == 0x258 && offsetof(SceAppInstallStatusInstalled, error_info) == 0x3c && offsetof(SceAppInstallStatusInstalled, local_copy_percent) == 0x250, "install status ABI");
static int sceAppInstUtilInitialize(void);
static int sceAppInstUtilInstallByPackage(MetaInfo *, SceAppInstallPkgInfo *, PlayGoInfo *) __attribute__((unused));
static int sceAppInstUtilGetInstallStatus(const char *, SceAppInstallStatusInstalled *) __attribute__((unused));
static int sceAppInstUtilAppInstallTitleDir(const char *title_id, const char *base_path, void *reserved);

typedef struct Segment { uint64_t offset; uint64_t length; bool finished; struct Segment *next; } Segment;
typedef struct Transfer {
    char path[MAX_PATH_BYTES + 1];
    uint64_t total;
    uint64_t completed;
    bool completion_notified;
    bool verified;
    unsigned active_lanes;
    Segment *segments;
    int fd;
    struct Transfer *next;
} Transfer;
typedef struct { Transfer *transfer; Segment *segment; uint64_t offset; uint64_t expected; uint64_t received; bool ended; char path[MAX_PATH_BYTES + 1]; } Lane;
static pthread_mutex_t g_transfer_lock = PTHREAD_MUTEX_INITIALIZER;
static Transfer *g_transfers;
static unsigned g_transfer_count;
static unsigned g_upload_lanes;
static atomic_uint g_clients;
static atomic_bool g_stopping;
static atomic_bool g_stop_pending;
static pthread_mutex_t g_operation_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t g_clients_lock = PTHREAD_MUTEX_INITIALIZER;
static int g_client_fds[MAX_CLIENTS];
static pthread_t g_client_threads[MAX_CLIENTS];
static bool g_client_used[MAX_CLIENTS];
static pthread_mutex_t g_mount_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t g_install_lock = PTHREAD_MUTEX_INITIALIZER;
static bool g_install_success_notified;
static bool g_install_failure_notified;
/* Reported by GET_CONFIG without the install lock. */
static atomic_bool g_appinst_init_attempted;
static atomic_int g_appinst_init_rc;
static atomic_bool g_authid_attempted;
static atomic_int g_authid_rc;
/* Dump registration is calling AppInst inside this process. */
static atomic_bool g_receiver_appinst;
static uint64_t g_original_authid;
static bool g_hold_system_authid;
static int g_port = DEFAULT_PORT;
static pthread_mutex_t g_notify_lock = PTHREAD_MUTEX_INITIALIZER;
static char g_last_incoming[16];
static char g_delivery_title[256];
static char g_delivery_icon[1024];
static void trace_mark(const char *tag, const char *detail);
void sspi_install_trace(const char *stage, const char *detail) { trace_mark(stage, detail); }

/* The loader process may not support every shell service. Resolve those only
   when requested, after the core receiver has started. Serialize the SDK loader. */
static void *sspi_load_optional_service(const char *module) {
    char stage[320];
    snprintf(stage, sizeof(stage), "optional library begin: %s", module);
    trace_mark("service", stage);
    sspi_startup_note(stage, 0);
    void *handle = dlopen(module, RTLD_NOW | RTLD_LOCAL);
    if (handle) {
        snprintf(stage, sizeof(stage), "optional library ready: %s", module);
    } else {
        const char *error = dlerror();
        snprintf(stage, sizeof(stage), "optional library unavailable: %.64s: %.192s",
                 module, error && *error ? error : "no loader diagnostic");
    }
    trace_mark("service", stage);
    sspi_startup_note(stage, handle ? 0 : -ENOSYS);
    return handle;
}

void *sspi_service_symbol(const char *module, const char *symbol) {
    static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
    static void *ipmi;
    static bool ipmi_attempted;
    static struct { const char *name; void *handle; bool attempted; } modules[] = {
        { "libSceAppInstUtil.sprx", NULL, false },
        { "libSceNotification.sprx", NULL, false },
        { "libSceSystemService.sprx", NULL, false },
    };
    void *address = NULL;
    pthread_mutex_lock(&lock);
    for (size_t i = 0; i < sizeof(modules) / sizeof(modules[0]); i++) {
        if (strcmp(module, modules[i].name)) continue;
        if (!modules[i].attempted) {
            modules[i].attempted = true;
            if (!strcmp(module, "libSceAppInstUtil.sprx")) {
                /* SDK issue #26: Sony's loader can stop if AppInstUtil starts
                   before Ipmi. Keep both modules loaded until process exit. */
                if (!ipmi_attempted) {
                    ipmi_attempted = true;
                    ipmi = sspi_load_optional_service("libSceIpmi.sprx");
                }
                if (!ipmi) {
                    const char *stage = "optional library blocked: libSceAppInstUtil.sprx requires libSceIpmi.sprx";
                    trace_mark("service", stage);
                    sspi_startup_note(stage, -ENOSYS);
                    break;
                }
            }
            modules[i].handle = sspi_load_optional_service(module);
        }
        if (modules[i].handle) {
            address = dlsym(modules[i].handle, symbol);
            if (!address) {
                char stage[320];
                const char *error = dlerror();
                snprintf(stage, sizeof(stage), "optional symbol unavailable: %.64s %.64s: %.128s",
                         module, symbol, error && *error ? error : "no loader diagnostic");
                trace_mark("service", stage);
                sspi_startup_note(stage, -ENOSYS);
            }
        }
        break;
    }
    pthread_mutex_unlock(&lock);
    return address;
}

static int sceAppInstUtilInitialize(void) {
    int (*call)(void) = sspi_service_symbol("libSceAppInstUtil.sprx", "sceAppInstUtilInitialize");
    return call ? call() : -ENOSYS;
}
static int sceAppInstUtilInstallByPackage(MetaInfo *meta, SceAppInstallPkgInfo *pkg, PlayGoInfo *playgo) {
    int (*call)(MetaInfo *, SceAppInstallPkgInfo *, PlayGoInfo *) = sspi_service_symbol("libSceAppInstUtil.sprx", "sceAppInstUtilInstallByPackage");
    return call ? call(meta, pkg, playgo) : -ENOSYS;
}
static int sceAppInstUtilGetInstallStatus(const char *content, SceAppInstallStatusInstalled *status) {
    int (*call)(const char *, SceAppInstallStatusInstalled *) = sspi_service_symbol("libSceAppInstUtil.sprx", "sceAppInstUtilGetInstallStatus");
    return call ? call(content, status) : -ENOSYS;
}
static int sceAppInstUtilAppInstallTitleDir(const char *title, const char *base, void *reserved) {
    int (*call)(const char *, const char *, void *) = sspi_service_symbol("libSceAppInstUtil.sprx", "sceAppInstUtilAppInstallTitleDir");
    return call ? call(title, base, reserved) : -ENOSYS;
}
static int sceNotificationSend(int user_id, bool is_logged, const char *payload) {
    int (*call)(int, bool, const char *) = sspi_service_symbol("libSceNotification.sprx", "sceNotificationSend");
    return call ? call(user_id, is_logged, payload) : -ENOSYS;
}

static uint64_t read_u64le(const uint8_t *p) { uint64_t v = 0; for (unsigned i = 0; i < 8; ++i) v |= (uint64_t)p[i] << (i * 8); return v; }
static uint32_t read_u32le(const uint8_t *p) { return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24); }
static void write_u32le(uint8_t *p, uint32_t v) { p[0] = v; p[1] = v >> 8; p[2] = v >> 16; p[3] = v >> 24; }
static void notify(const char *message) {
    Notification n; memset(&n, 0, sizeof(n));
    n.target_id = -1;
    n.use_icon_uri = 1;
    pthread_mutex_lock(&g_notify_lock);
    snprintf(n.message, sizeof(n.message), "%s%s%s", message, g_delivery_title[0] ? "\n" : "", g_delivery_title);
    snprintf(n.uri, sizeof(n.uri), "%s", g_delivery_icon[0] ? g_delivery_icon : "cxml://psnotification/tex_icon_system");
    pthread_mutex_unlock(&g_notify_lock);
    trace_mark("notify begin", message);
    int rc = sceKernelSendNotificationRequest(0, &n, sizeof(n), 0);
    char detail[80]; snprintf(detail, sizeof(detail), "rc=0x%08X", (unsigned)rc);
    trace_mark("notify end", detail);
}
static void trace_resources(const char *tag) {
    char detail[128];
    pthread_mutex_lock(&g_transfer_lock);
    snprintf(detail, sizeof(detail), "clients=%u lanes=%u transfers=%u",
             atomic_load(&g_clients), g_upload_lanes, g_transfer_count);
    pthread_mutex_unlock(&g_transfer_lock);
    trace_mark(tag, detail);
}
/* Crash-site trace: append-only markers in /data/SSPI/sspi.log. Deliberately
   NOT used on the per-chunk hot path. Fetch via FTP after a receiver death;
   the last line is where it died. The newest 512 KiB and one previous file
   are kept, so a failure storm cannot fill /data. */
#define TRACE_LOG "/data/SSPI/sspi.log"
#define TRACE_PREVIOUS "/data/SSPI/sspi.previous.log"
#define TRACE_LIMIT (512 * 1024)
static void trace_mark(const char *tag, const char *detail) {
    static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
    char line[576];
    int n = snprintf(line, sizeof(line), "%s %s\n", tag, detail ? detail : "");
    if (n <= 0) return;
    if ((size_t)n >= sizeof(line)) { n = sizeof(line) - 1; line[n - 1] = '\n'; }
    pthread_mutex_lock(&lock);
    int fd = open(TRACE_LOG, O_WRONLY | O_CREAT | O_APPEND, 0664);
    struct stat st;
    if (fd >= 0 && !fstat(fd, &st) && st.st_size >= TRACE_LIMIT) {
        close(fd);
        fd = open(TRACE_LOG, O_WRONLY | O_CREAT | O_APPEND | (rename(TRACE_LOG, TRACE_PREVIOUS) ? O_TRUNC : 0), 0664);
    }
    if (fd >= 0) {
        size_t off = 0;
        while (off < (size_t)n) {
            ssize_t w = write(fd, line + off, (size_t)n - off);
            if (w < 0 && errno == EINTR) continue;
            if (w <= 0) break;
            off += (size_t)w;
        }
        fsync(fd);
        close(fd);
    }
    pthread_mutex_unlock(&lock);
}
static void notify_incoming(const char *path) {
    char tid[16] = {0};
    const char *scan = NULL;
    if (!strncmp(path, "/data/homebrew/.sspi-incoming/", 30)) scan = path + 30;
    else if (!strncmp(path, "/data/homebrew/backports/", 25)) scan = path + 25;
    else if (!strncmp(path, "/data/homebrew/", 15)) scan = path + 15;
    else if (!strncmp(path, "/user/data/tmp/upload_", 22)) scan = path + 22;
    if (scan) {
        size_t i = 0;
        while (i < 9 && scan[i] && scan[i] != '/' && scan[i] != '_') { tid[i] = scan[i]; i++; }
        tid[i] = 0;
    }
    pthread_mutex_lock(&g_notify_lock);
    if (tid[0] && strcmp(g_last_incoming, tid) == 0) { pthread_mutex_unlock(&g_notify_lock); return; }
    if (tid[0]) { strncpy(g_last_incoming, tid, sizeof(g_last_incoming) - 1); g_last_incoming[sizeof(g_last_incoming) - 1] = 0; }
    pthread_mutex_unlock(&g_notify_lock);
    char note[192];
    const char *leaf = strrchr(path, '/');
    leaf = leaf ? leaf + 1 : path;
    if (tid[0] && !strncmp(path, "/data/homebrew/", 15)) snprintf(note, sizeof(note), "SSPI incoming title %s", tid);
    else if (tid[0]) snprintf(note, sizeof(note), "SSPI incoming PKG %s", tid);
    else snprintf(note, sizeof(note), "SSPI incoming %s", leaf);
    notify(note);
}
static int send_all(int fd, const void *data, size_t n) { const uint8_t *p = data; while (n) { ssize_t w = send(fd, p, n, 0); if (w < 0 && errno == EINTR) continue; if (w <= 0) return -1; p += w; n -= (size_t)w; } return 0; }
static int recv_all(int fd, void *data, size_t n) { uint8_t *p = data; while (n) { ssize_t r = recv(fd, p, n, 0); if (r < 0 && errno == EINTR) continue; if (r <= 0) return -1; p += r; n -= (size_t)r; } return 0; }
static int reply(int fd, uint8_t code, const void *body, uint32_t size) { uint8_t header[5]; header[0] = code; write_u32le(header + 1, size); return send_all(fd, header, sizeof(header)) || (size && send_all(fd, body, size)) ? -1 : 0; }
static int text_reply(int fd, uint8_t code, const char *text) { return reply(fd, code, text, (uint32_t)strlen(text)); }

/* FW 10+ AppInst calls require the SYSTEM install AuthID. Keep this lazy: payload
 * startup must stay usable even when the target's AppInst environment is absent. */
static int prepare_appinst_authid(uint64_t *saved_authid) {
    /* The native loader temporarily uses debugger credentials on this process. */
    if (sspi_install_loading()) return -EBUSY;
    pid_t pid = getpid();
    uint64_t current = kernel_get_ucred_authid(pid);
    g_authid_attempted = true;
    if (!current) return g_authid_rc = -1;
    if (!g_original_authid && current != SYSTEM_INSTALL_AUTHID) g_original_authid = current;
    *saved_authid = g_original_authid ? g_original_authid : current;
    if (current != SYSTEM_INSTALL_AUTHID && kernel_set_ucred_authid(pid, SYSTEM_INSTALL_AUTHID)) return g_authid_rc = -2;
    return kernel_get_ucred_authid(pid) == SYSTEM_INSTALL_AUTHID ? (g_authid_rc = 0) : (g_authid_rc = -3);
}
static int restore_appinst_authid(uint64_t saved_authid) {
    if (!saved_authid || saved_authid == SYSTEM_INSTALL_AUTHID) return 0;
    trace_mark("auth restore begin", "");
    pid_t pid = getpid();
    for (int attempt = 0; attempt < 3; ++attempt) {
        kernel_set_ucred_authid(pid, saved_authid);
        if (kernel_get_ucred_authid(pid) == saved_authid) { trace_mark("auth restore end", "ok"); return 0; }
    }
    trace_mark("auth restore end", "failed");
    return g_authid_rc = -4;
}
/* Dump registration still calls AppInst in this process. Credentials change under
   the install lock, but the native calls run without it: a stalled service must
   not block preflight, status, configuration or STOP. Installer launches wait for
   the mark to clear, because the loader also swaps this process's credentials. */
static int begin_receiver_appinst(uint64_t *saved_authid) {
    pthread_mutex_lock(&g_install_lock);
    int rc = prepare_appinst_authid(saved_authid);
    if (!rc) atomic_store(&g_receiver_appinst, true);
    pthread_mutex_unlock(&g_install_lock);
    return rc;
}
static int end_receiver_appinst(uint64_t saved_authid) {
    pthread_mutex_lock(&g_install_lock);
    int rc = g_hold_system_authid ? 0 : restore_appinst_authid(saved_authid);
    atomic_store(&g_receiver_appinst, false);
    pthread_mutex_unlock(&g_install_lock);
    return rc;
}
/* Callers hold g_mount_lock. */
static int initialize_appinst_locked(void) {
    if (!g_appinst_init_attempted) {
        trace_mark("appinst init begin", "");
        g_appinst_init_rc = sceAppInstUtilInitialize(); g_appinst_init_attempted = true;
        char detail[64]; snprintf(detail, sizeof(detail), "rc=0x%08X", (unsigned)g_appinst_init_rc);
        trace_mark("appinst init end", detail);
    }
    return g_appinst_init_rc;
}
static int ensure_appinst_ready(void) {
    uint64_t saved_authid = 0;
    int auth_rc = begin_receiver_appinst(&saved_authid);
    if (!auth_rc) initialize_appinst_locked();
    int restore_rc = end_receiver_appinst(saved_authid);
    return auth_rc ? auth_rc : (g_appinst_init_rc ? g_appinst_init_rc : restore_rc);
}
static int install_error_reply(int fd, int api_code, const char *stage, const char *path) {
    char reply_text[512];
    snprintf(reply_text, sizeof(reply_text), "{\"api_code\":%d,\"state\":\"failed\",\"stage\":\"%s\",\"path\":\"%s\",\"error\":\"%s\"}", api_code, stage, path ? path : "", stage);
    return text_reply(fd, RESP_ERROR, reply_text);
}

static int is_tid(const char *s) {
    if (!s || strlen(s) != 9) return 0;
    if (strncmp(s, "CUSA", 4) && strncmp(s, "PPSA", 4)) return 0;
    for (int i = 4; i < 9; ++i) if (s[i] < '0' || s[i] > '9') return 0;
    return 1;
}
static int remount_system_ex(void) {
    struct iovec iov[] = {
        IOVEC_ENTRY("from"), IOVEC_ENTRY("/dev/ssd0.system_ex"),
        IOVEC_ENTRY("fspath"), IOVEC_ENTRY("/system_ex"),
        IOVEC_ENTRY("fstype"), IOVEC_ENTRY("exfatfs"),
        IOVEC_ENTRY("large"), IOVEC_ENTRY("yes"),
        IOVEC_ENTRY("timezone"), IOVEC_ENTRY("static"),
        IOVEC_ENTRY("async"), { NULL, 0 },
        IOVEC_ENTRY("ignoreacl"), { NULL, 0 },
    };
    return nmount(iov, IOVEC_SIZE(iov), MNT_UPDATE);
}
static int mount_nullfs(const char *src, const char *dst) {
    struct iovec iov[] = {
        IOVEC_ENTRY("fstype"), IOVEC_ENTRY("nullfs"),
        IOVEC_ENTRY("from"), IOVEC_ENTRY(src),
        IOVEC_ENTRY("fspath"), IOVEC_ENTRY(dst),
    };
    return nmount(iov, IOVEC_SIZE(iov), 0);
}
static int path_is_nullfs(const char *path) {
    struct statfs sfs;
    if (statfs(path, &sfs) != 0) return 0;
    return strcmp(sfs.f_fstypename, "nullfs") == 0;
}
static int copy_file(const char *src, const char *dst) {
    int in = open(src, O_RDONLY);
    if (in < 0) return -1;
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0775);
    if (out < 0) { close(in); return -1; }
    char buf[65536];
    ssize_t n;
    int rc = 0;
    while ((n = read(in, buf, sizeof(buf))) > 0) {
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = write(out, buf + off, (size_t)(n - off));
            if (w <= 0) { rc = -1; goto done; }
            off += w;
        }
    }
    if (n < 0) rc = -1;
done:
    close(in);
    close(out);
    return rc;
}
static int copy_dir(const char *src, const char *dst) {
    if (mkdir(dst, 0755) && errno != EEXIST) return -1;
    DIR *d = opendir(src);
    if (!d) return -1;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        char ss[PATH_MAX], dd[PATH_MAX];
        snprintf(ss, sizeof(ss), "%s/%s", src, e->d_name);
        snprintf(dd, sizeof(dd), "%s/%s", dst, e->d_name);
        struct stat st;
        /* lstat: a linked directory loop would otherwise recurse until the stack overflows. */
        if (lstat(ss, &st) != 0) continue;
        if (S_ISDIR(st.st_mode)) { if (copy_dir(ss, dd)) { closedir(d); return -1; } }
        else if (S_ISREG(st.st_mode)) { if (copy_file(ss, dd)) { closedir(d); return -1; } }
    }
    closedir(d);
    return 0;
}
static int is_appmeta_file(const char *name) {
    if (!strcasecmp(name, "param.json") || !strcasecmp(name, "param.sfo")) return 1;
    const char *ext = strrchr(name, '.');
    return ext && (!strcasecmp(ext, ".png") || !strcasecmp(ext, ".dds") || !strcasecmp(ext, ".at9"));
}
static int copy_appmeta(const char *src_sce, const char *title_id) {
    char dst[PATH_MAX];
    mkdir("/user/appmeta", 0777);
    snprintf(dst, sizeof(dst), "/user/appmeta/%s", title_id);
    mkdir(dst, 0755);
    DIR *d = opendir(src_sce);
    if (!d) return -1;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (!is_appmeta_file(e->d_name)) continue;
        char ss[PATH_MAX], dd[PATH_MAX];
        snprintf(ss, sizeof(ss), "%s/%s", src_sce, e->d_name);
        snprintf(dd, sizeof(dd), "%s/%s", dst, e->d_name);
        struct stat st;
        if (stat(ss, &st) == 0 && S_ISREG(st.st_mode) && copy_file(ss, dd)) { closedir(d); return -1; }
    }
    closedir(d);
    return 0;
}
static int json_string(const char *json, const char *key, char *out, size_t out_size) {
    char search[64];
    snprintf(search, sizeof(search), "\"%s\"", key);
    const char *p = strstr(json, search);
    if (!p) return -1;
    p = strchr(p + strlen(search), ':');
    if (!p) return -1;
    while (*++p && (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')) {}
    if (*p != '"') return -1;
    p++;
    size_t i = 0;
    while (i + 1 < out_size && p[i] && p[i] != '"') { out[i] = p[i]; i++; }
    out[i] = 0;
    return 0;
}
static int read_title_from_json(const char *path, char *title_id, size_t size) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return -1; }
    long len = ftell(f);
    if (len <= 0 || len > 1024 * 1024) { fclose(f); return -1; }
    if (fseek(f, 0, SEEK_SET) != 0) { fclose(f); return -1; }
    char *buf = (char *)malloc((size_t)len + 1);
    if (!buf) { fclose(f); return -1; }
    size_t n = fread(buf, 1, (size_t)len, f);
    fclose(f);
    buf[n] = 0;
    int rc = -1;
    if (json_string(buf, "titleId", title_id, size) == 0 || json_string(buf, "title_id", title_id, size) == 0) {
        title_id[strcspn(title_id, "\r\n")] = 0;
        rc = is_tid(title_id) ? 0 : -1;
    }
    free(buf);
    return rc;
}
static int fix_drm_type(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return -1; }
    long len = ftell(f);
    if (len <= 0 || len > 1024 * 1024) { fclose(f); return -1; }
    if (fseek(f, 0, SEEK_SET) != 0) { fclose(f); return -1; }
    char *buf = (char *)malloc((size_t)len + 1);
    if (!buf) { fclose(f); return -1; }
    size_t n = fread(buf, 1, (size_t)len, f);
    fclose(f);
    buf[n] = 0;
    char *p = strstr(buf, "\"applicationDrmType\"");
    if (!p) { free(buf); return 0; }
    char *colon = strchr(p, ':');
    char *q1 = colon ? strchr(colon, '"') : NULL;
    char *q2 = q1 ? strchr(q1 + 1, '"') : NULL;
    if (!q1 || !q2) { free(buf); return -1; }
    if ((size_t)(q2 - q1 - 1) == 8 && !strncmp(q1 + 1, "standard", 8)) { free(buf); return 0; }
    size_t prefix = (size_t)(q1 - buf + 1);
    size_t suffix = strlen(q2);
    char *out = (char *)malloc(prefix + 8 + suffix + 1);
    if (!out) { free(buf); return -1; }
    memcpy(out, buf, prefix);
    memcpy(out + prefix, "standard", 8);
    memcpy(out + prefix + 8, q2, suffix + 1);
    f = fopen(path, "wb");
    if (!f) { free(buf); free(out); return -1; }
    size_t wrote = fwrite(out, 1, strlen(out), f);
    (void)wrote;
    fclose(f);
    free(buf);
    free(out);
    return 1;
}
static int mount_dump(const char *game_path, char *err, size_t err_size) {
    char title_id[12] = {0};
    char sce_sys[PATH_MAX], param_json[PATH_MAX], system_ex[PATH_MAX], user_app[PATH_MAX], user_sce[PATH_MAX], lnk[PATH_MAX];
    struct stat st;
    snprintf(sce_sys, sizeof(sce_sys), "%s/sce_sys", game_path);
    if (stat(sce_sys, &st) || !S_ISDIR(st.st_mode)) {
        snprintf(err, err_size, "missing sce_sys in %s; this is an overlay, not a mountable dump", game_path);
        return -1;
    }
    snprintf(param_json, sizeof(param_json), "%s/param.json", sce_sys);
    if (read_title_from_json(param_json, title_id, sizeof(title_id))) {
        const char *slash = strrchr(game_path, '/');
        const char *leaf = slash ? slash + 1 : game_path;
        if (is_tid(leaf)) snprintf(title_id, sizeof(title_id), "%s", leaf);
        else { snprintf(err, err_size, "no title ID in param.json"); return -1; }
    }
    {
        const char *slash = strrchr(game_path, '/');
        const char *leaf = slash ? slash + 1 : game_path;
        if (is_tid(leaf) && title_id[0] && strcmp(leaf, title_id)) {
            snprintf(err, err_size, "dump title %s does not match requested %s", title_id, leaf);
            return -1;
        }
    }
    fix_drm_type(param_json);
    trace_mark("mount remount_system_ex", game_path);
    remount_system_ex();
    snprintf(system_ex, sizeof(system_ex), "/system_ex/app/%s", title_id);
    mkdir("/system_ex/app", 0755);
    mkdir(system_ex, 0755);
    if (path_is_nullfs(system_ex)) unmount(system_ex, 0);
    trace_mark("mount nullfs", game_path);
    if (mount_nullfs(game_path, system_ex)) {
        snprintf(err, err_size, "nullfs mount failed (%d)", errno);
        return -1;
    }
    snprintf(user_app, sizeof(user_app), "/user/app/%s", title_id);
    snprintf(user_sce, sizeof(user_sce), "%s/sce_sys", user_app);
    mkdir("/user/app", 0755);
    mkdir(user_app, 0755);
    mkdir(user_sce, 0755);
    if (copy_dir(sce_sys, user_sce)) { unmount(system_ex, 0); snprintf(err, err_size, "sce_sys copy failed"); return -1; }
    if (copy_appmeta(sce_sys, title_id)) { unmount(system_ex, 0); snprintf(err, err_size, "appmeta copy failed"); return -1; }
    trace_mark("mount appinst_ready", title_id);
    int ready = ensure_appinst_ready();
    if (ready) {
        unmount(system_ex, 0);
        snprintf(err, err_size, ready == -EBUSY ? "AppInst is busy starting a package installer (%d); retry" : "AppInst not ready (%d)", ready);
        return -1;
    }
    uint64_t saved = 0;
    int auth = begin_receiver_appinst(&saved);
    trace_mark("mount AppInstallTitleDir", title_id);
    int rc = auth ? auth : sceAppInstUtilAppInstallTitleDir(title_id, "/user/app/", NULL);
    end_receiver_appinst(saved);
    if (rc) {
        unmount(system_ex, 0);
        snprintf(err, err_size, "AppInstallTitleDir failed (%d)", rc);
        return -1;
    }
    trace_mark("mount ok", title_id);
    snprintf(lnk, sizeof(lnk), "/user/app/%s/mount.lnk", title_id);
    FILE *lf = fopen(lnk, "w");
    if (lf) { fprintf(lf, "%s", game_path); fclose(lf); }
    char check[PATH_MAX];
    struct stat cst;
    snprintf(check, sizeof(check), "/user/app/%s/sce_sys/param.json", title_id);
    if (stat(check, &cst) || !S_ISREG(cst.st_mode)) {
        unmount(system_ex, 0);
        snprintf(err, err_size, "registration incomplete: /user/app/%s/sce_sys is empty", title_id);
        return -1;
    }
    return 0;
}
static int handle_mount_game(int fd, const char *body) {
    char path[PATH_MAX];
    if (!body || !*body) return text_reply(fd, RESP_ERROR, "missing dump path or title ID");
    trace_mark("mount request", body);
    if (body[0] == '/') {
        if (strncmp(body, "/data/homebrew/", 15) || strstr(body, "..") || strchr(body, '\\') || strlen(body) >= sizeof(path))
            return text_reply(fd, RESP_ERROR, "path rejected");
        snprintf(path, sizeof(path), "%s", body);
        size_t n = strlen(path);
        while (n && path[n - 1] == '/') path[--n] = 0;
    } else {
        if (!is_tid(body)) return text_reply(fd, RESP_ERROR, "invalid title ID");
        char main_path[PATH_MAX], bp_path[PATH_MAX];
        snprintf(main_path, sizeof(main_path), "/data/homebrew/%s", body);
        snprintf(bp_path, sizeof(bp_path), "/data/homebrew/backports/%s", body);
        struct stat st_main, st_bp;
        int main_ok = !stat(main_path, &st_main) && S_ISDIR(st_main.st_mode);
        int bp_ok = !stat(bp_path, &st_bp) && S_ISDIR(st_bp.st_mode);
        if (main_ok && bp_ok) return text_reply(fd, RESP_ERROR, "both /data/homebrew and backports exist; pass the full path");
        snprintf(path, sizeof(path), "%s", main_ok ? main_path : bp_path);
    }
    struct stat st;
    if (stat(path, &st) || !S_ISDIR(st.st_mode)) return text_reply(fd, RESP_ERROR, "dump folder not found");
    char err[256] = {0};
    trace_resources("mount resources");
    /* A stalled AppInst call keeps this lock; answer instead of queueing a thread behind it. */
    if (pthread_mutex_trylock(&g_mount_lock))
        return text_reply(fd, RESP_ERROR, "Another dump registration is still running on the console; retry after it finishes or reload the receiver");
    int mount_rc = mount_dump(path, err, sizeof(err));
    pthread_mutex_unlock(&g_mount_lock);
    if (mount_rc) {
        trace_mark("mount rejected", err);
        notify("SSPI dump mount failed");
        return text_reply(fd, RESP_ERROR, err[0] ? err : "dump mount failed");
    }
    notify("SSPI dump mounted");
    char ok[320];
    snprintf(ok, sizeof(ok), "OK mounted %s", path);
    return text_reply(fd, RESP_OK, ok);
}

static bool allowed_path(const char *path) {
    if (!path || !*path || strlen(path) > MAX_PATH_BYTES || strstr(path, "..") || strchr(path, '\\')) return false;
    return (strncmp(path, "/user/data/tmp/upload_", 22) == 0 && strlen(path) > 26 && !strcmp(path + strlen(path) - 4, ".pkg")) || strncmp(path, "/data/homebrew/", 15) == 0;
}
static int mkdir_parents(const char *path) {
    char copy[MAX_PATH_BYTES + 1]; size_t len = strlen(path); if (len >= sizeof(copy)) return -1; memcpy(copy, path, len + 1);
    for (char *p = copy + 1; *p; ++p) if (*p == '/') { struct stat st; *p = 0; if (lstat(copy, &st) == 0) { if (!S_ISDIR(st.st_mode) || S_ISLNK(st.st_mode)) return -1; } else if (errno != ENOENT || (mkdir(copy, 0775) && errno != EEXIST) || lstat(copy, &st) || !S_ISDIR(st.st_mode) || S_ISLNK(st.st_mode)) return -1; *p = '/'; }
    return 0;
}
static bool final_path_is_symlink(const char *path) { struct stat st; return lstat(path, &st) == 0 && S_ISLNK(st.st_mode); }
static Transfer *find_transfer(const char *path) { for (Transfer *t = g_transfers; t; t = t->next) if (!strcmp(t->path, path)) return t; return NULL; }
static void destroy_transfer_locked(Transfer *target) {
    Transfer **link = &g_transfers;
    while (*link && *link != target) link = &(*link)->next;
    if (!*link || target->active_lanes) return;
    *link = target->next;
    if (target->fd >= 0) close(target->fd);
    while (target->segments) { Segment *next = target->segments->next; free(target->segments); target->segments = next; }
    free(target);
    g_transfer_count--;
}
/* Completed records end at VERIFY or at install acceptance. A sender that gave up
   before either must not exhaust the table: reuse the oldest idle completed one. */
static bool evict_completed_transfer_locked(void) {
    Transfer *oldest = NULL;
    for (Transfer *t = g_transfers; t; t = t->next) if (!t->active_lanes && t->completed == t->total) oldest = t;
    if (oldest) destroy_transfer_locked(oldest);
    return oldest != NULL;
}
static void release_lane(Lane *lane) {
    if (!lane->transfer) return;
    pthread_mutex_lock(&g_transfer_lock);
    Transfer *t = lane->transfer;
    if (!lane->ended) { t->active_lanes--; g_upload_lanes--; }
    lane->transfer = NULL;
    lane->segment = NULL;
    char discard[MAX_PATH_BYTES + 32] = "";
    if (!t->active_lanes && t->completed != t->total) {
        /* Nothing installs or resumes a partial PKG (every attempt restarts at offset
           0), and abandoned ones filled the console. Move the name aside under the
           lock so a retry can recreate it, then reclaim the space outside the lock. */
        static unsigned discarded;
        char partial[MAX_PATH_BYTES + 1] = "";
        if (!strncmp(t->path, "/user/data/tmp/upload_", 22)) snprintf(partial, sizeof(partial), "%s", t->path);
        destroy_transfer_locked(t);
        if (partial[0]) {
            snprintf(discard, sizeof(discard), "%s.%u.discard", partial, ++discarded);
            if (rename(partial, discard)) discard[0] = 0;
        }
    }
    pthread_mutex_unlock(&g_transfer_lock);
    if (discard[0]) unlink(discard);
}

static int config_load(void) { FILE *f = fopen("/data/SSPI/config.ini", "r"); if (!f) return DEFAULT_PORT; char line[64]; int port = DEFAULT_PORT; if (fgets(line, sizeof(line), f) && sscanf(line, "port=%d", &port) == 1 && port >= 1024 && port <= 65535) {} else port = DEFAULT_PORT; fclose(f); return port; }
static int config_save(int port) { const char *tmp = "/data/SSPI/config.ini.tmp"; FILE *f = fopen(tmp, "w"); if (!f) return -1; if (fprintf(f, "port=%d\n", port) < 0 || fflush(f) || fsync(fileno(f))) { fclose(f); unlink(tmp); return -1; } if (fclose(f) || rename(tmp, "/data/SSPI/config.ini")) { unlink(tmp); return -1; } return 0; }

static int handle_start(int fd, const uint8_t *body, uint32_t size, Lane *lane) {
    if (lane->transfer) return text_reply(fd, RESP_ERROR, "lane already started");
    if (!body || size < 25) return text_reply(fd, RESP_ERROR, "invalid START_UPLOAD");
    const uint8_t *nul = memchr(body, 0, size); if (!nul || size < (uint32_t)(nul - body + 1 + 24)) return text_reply(fd, RESP_ERROR, "invalid START_UPLOAD");
    size_t path_len = (size_t)(nul - body); if (!path_len || path_len > MAX_PATH_BYTES || (uint32_t)(path_len + 25) != size) return text_reply(fd, RESP_ERROR, "invalid START_UPLOAD size");
    char path[MAX_PATH_BYTES + 1]; memcpy(path, body, path_len); path[path_len] = 0; if (!allowed_path(path)) return text_reply(fd, RESP_ERROR, "path rejected");
    uint64_t total = read_u64le(nul + 1), offset = read_u64le(nul + 9), segment = read_u64le(nul + 17);
    if (offset > total || segment > total - offset || ((total == 0 || segment == 0) && !(total == 0 && offset == 0 && segment == 0))) return text_reply(fd, RESP_ERROR, "invalid segment");
    pthread_mutex_lock(&g_transfer_lock);
    if (g_upload_lanes >= MAX_UPLOAD_LANES) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "receiver upload capacity busy; retry"); }
    Transfer *t = find_transfer(path);
    if (t && offset == 0) {
        /* Never free under live lanes (a retry overlapping dying threads would
           leave them with a dangling transfer pointer and corrupt the heap). */
        if (t->active_lanes) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "previous attempt still closing; retry"); }
        destroy_transfer_locked(t); t = NULL;
    }
    if (!t) {
        if (g_transfer_count >= MAX_TRANSFERS && !evict_completed_transfer_locked()) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "receiver transfer capacity busy; verify completed files"); }
        if (offset != 0) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "lane 0 must create file first"); }
        if (mkdir_parents(path)) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "could not create parent directories"); }
        if (final_path_is_symlink(path)) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "symlink target rejected"); }
        int file = open(path, O_CREAT | O_TRUNC | O_RDWR | O_NOFOLLOW, strncmp(path, "/data/homebrew/", 15) == 0 ? 0775 : 0664); if (file < 0) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "create failed"); }
        if (strncmp(path, "/data/homebrew/", 15) == 0) fchmod(file, 0775);
        if (total && ftruncate(file, (off_t)total)) { close(file); pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "preallocate failed (console storage full?)"); }
        t = calloc(1, sizeof(*t)); if (!t) { close(file); pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "out of memory"); }
        strcpy(t->path, path); t->total = total; t->fd = file; t->next = g_transfers; g_transfers = t; g_transfer_count++;
    }
    if (t->total != total) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "total size mismatch"); }
    for (Segment *s = t->segments; s; s = s->next) if (offset < s->offset + s->length && s->offset < offset + segment) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "overlapping segment"); }
    Segment *new_segment = calloc(1, sizeof(*new_segment)); if (!new_segment) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "out of memory"); }
    new_segment->offset = offset; new_segment->length = segment; new_segment->next = t->segments; t->segments = new_segment; t->active_lanes++; g_upload_lanes++;
    lane->transfer = t; lane->segment = new_segment; lane->offset = offset; lane->expected = segment; lane->received = 0; lane->ended = false; strcpy(lane->path, path); pthread_mutex_unlock(&g_transfer_lock);
    if (offset == 0) notify_incoming(path);
    return text_reply(fd, RESP_READY, "READY");
}
static int handle_upload_chunk(int fd, uint32_t size, Lane *lane, uint8_t *buffer) {
    if (!lane->transfer || lane->ended || !size || size > MAX_FRAME ||
        (uint64_t)size > lane->expected - lane->received) {
        text_reply(fd, RESP_ERROR, "invalid chunk");
        return -1;
    }
    // Keep the 8 MiB wire protocol, but never allocate an 8 MiB frame per lane.
    uint32_t remaining = size;
    while (remaining) {
        size_t bytes = remaining < UPLOAD_BUFFER ? remaining : UPLOAD_BUFFER;
        if (recv_all(fd, buffer, bytes)) return -1;
        size_t done = 0;
        while (done < bytes) {
            ssize_t written = pwrite(lane->transfer->fd, buffer + done, bytes - done,
                                     (off_t)(lane->offset + lane->received + done));
            if (written < 0 && errno == EINTR) continue;
            if (written <= 0) {
                int code = written < 0 ? errno : EIO; char error[80];
                snprintf(error, sizeof(error), code == ENOSPC ? "console storage is full (pwrite errno %d)" : "pwrite failed (errno %d)", code);
                text_reply(fd, RESP_ERROR, error); return -1;
            }
            done += (size_t)written;
        }
        lane->received += bytes;
        remaining -= (uint32_t)bytes;
    }
    return text_reply(fd, RESP_OK, "OK");
}
static int handle_end(int fd, Lane *lane) {
    if (!lane->transfer || lane->ended || lane->received != lane->expected)
        return text_reply(fd, RESP_ERROR, "segment incomplete");
    Transfer *t = lane->transfer;
    if (strncmp(t->path, "/data/homebrew/", 15) != 0 && fsync(t->fd) != 0)
        return text_reply(fd, RESP_ERROR, "fsync failed");
    char done_path[MAX_PATH_BYTES + 1];
    pthread_mutex_lock(&g_transfer_lock);
    lane->ended = true;
    lane->segment->finished = true;
    t->completed += lane->expected;
    t->active_lanes--;
    g_upload_lanes--;
    bool complete = t->completed == t->total && !t->completion_notified;
    t->completion_notified |= complete;
    strcpy(done_path, t->path);
    // END releases every pointer before VERIFY or an offset-zero retry may free t.
    lane->transfer = NULL;
    lane->segment = NULL;
    pthread_mutex_unlock(&g_transfer_lock);
    if (complete && (strstr(done_path, ".pkg") || strstr(done_path, "eboot.bin") || strstr(done_path, ".exfat"))) notify("SSPI transfer complete");
    return text_reply(fd, RESP_OK, "OK");
}

static bool valid_package_fd(int fd, uint64_t size) {
    uint8_t header[0x80];
    if (size < sizeof(header) || pread(fd, header, sizeof(header), 0) != (ssize_t)sizeof(header)) return false;
    if (!memcmp(header, "\x7f" "FIH", 4)) {
        uint64_t image = read_u64le(header + 0x10), length = read_u64le(header + 0x18), cnt = read_u64le(header + 0x58);
        if (image < 0x10000 || !length || image > size || length > size - image || cnt < image + length || cnt > size || size - cnt < sizeof(header)) return false;
        if (pread(fd, header, sizeof(header), (off_t)cnt) != (ssize_t)sizeof(header)) return false;
    }
    return memcmp(header, "\x7f" "CNT", 4) == 0;
}

static int handle_verify(int fd, const char *path) {
    if (!allowed_path(path)) return text_reply(fd, RESP_ERROR, "path rejected");
    pthread_mutex_lock(&g_transfer_lock);
    Transfer *t = find_transfer(path); struct stat st; bool is_package_path = !strncmp(path, "/user/data/tmp/upload_", 22);
    if (!t || t->active_lanes != 0 || t->completed != t->total || fstat(t->fd, &st) || !S_ISREG(st.st_mode) || (uint64_t)st.st_size != t->total) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "file unavailable or transfer incomplete"); }
    if (is_package_path) { if (!valid_package_fd(t->fd, t->total)) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "PKG magic failed"); } t->verified = true; }
    uint64_t verified_size = (uint64_t)st.st_size;
    if (!is_package_path) {
        /* Flushing a large file can take a long time. Pin the record like a lane
           instead of holding the lock, so other lanes, STOP and installs continue. */
        t->active_lanes++;
        pthread_mutex_unlock(&g_transfer_lock);
        int synced = fsync(t->fd);
        pthread_mutex_lock(&g_transfer_lock);
        t->active_lanes--;
        if (synced) { pthread_mutex_unlock(&g_transfer_lock); return text_reply(fd, RESP_ERROR, "dump fsync failed"); }
        destroy_transfer_locked(t);
    }
    pthread_mutex_unlock(&g_transfer_lock);
    char message[128]; snprintf(message, sizeof(message), "OK {\"size\":%llu}", (unsigned long long)verified_size); return text_reply(fd, RESP_OK, message);
}
/* ShadowMount images upload into a dot folder that ShadowMount's scan skips; publishing
   renames the verified file into /data/homebrew so a scan never sees a partial image. */
#define IMAGE_STAGING "/data/homebrew/.sspi-incoming"
static bool valid_image_name(const char *name) {
    size_t n = strlen(name);
    if (n < 7 || n > 200 || name[0] == '.' || strstr(name, "..")) return false;
    for (size_t i = 0; i < n; ++i) if (name[i] < 0x21 || name[i] > 0x7e || name[i] == '/' || name[i] == '\\') return false;
    const char *ext = name + n - 6;
    return (ext[0] == '.') && (ext[1] | 32) == 'e' && (ext[2] | 32) == 'x' && (ext[3] | 32) == 'f' && (ext[4] | 32) == 'a' && (ext[5] | 32) == 't';
}
static int handle_image(int fd, const uint8_t *body, uint32_t size) {
    if (!body || size < 2 || memchr(body, 0, size)) return text_reply(fd, RESP_ERROR, "invalid image request");
    char op = (char)body[0]; const char *name = (const char *)body + 1;
    if ((op != 's' && op != 'p' && op != 'd') || !valid_image_name(name)) return text_reply(fd, RESP_ERROR, "image name rejected");
    char staged[MAX_PATH_BYTES + 1], final[MAX_PATH_BYTES + 1];
    snprintf(staged, sizeof(staged), IMAGE_STAGING "/%s", name);
    snprintf(final, sizeof(final), "/data/homebrew/%s", name);
    pthread_mutex_lock(&g_transfer_lock);
    bool active = find_transfer(staged) != NULL;
    struct stat image, part;
    bool has_image = lstat(final, &image) == 0, has_part = lstat(staged, &part) == 0;
    char message[MAX_PATH_BYTES + 160];
    int rc; bool published = false;
    if (op == 's') {
        struct statfs fs; unsigned long long free_bytes = statfs("/data", &fs) == 0 ? (unsigned long long)fs.f_bavail * (unsigned long long)fs.f_bsize : 0;
        snprintf(message, sizeof(message), "{\"image\":{\"exists\":%s,\"size\":%llu},\"staged\":{\"exists\":%s,\"size\":%llu,\"active\":%s},\"free\":%llu}",
            has_image ? "true" : "false", has_image ? (unsigned long long)image.st_size : 0ULL,
            has_part ? "true" : "false", has_part ? (unsigned long long)part.st_size : 0ULL, active ? "true" : "false", free_bytes);
        rc = text_reply(fd, RESP_DATA, message);
    } else if (active) {
        rc = text_reply(fd, RESP_ERROR, "image upload still active or unverified");
    } else if (op == 'd') {
        rc = !has_part || unlink(staged) == 0 ? text_reply(fd, RESP_OK, "OK") : text_reply(fd, RESP_ERROR, "staged image could not be removed");
    } else if (!has_part || !S_ISREG(part.st_mode)) {
        rc = text_reply(fd, RESP_ERROR, "staged image not found");
    } else if (has_image) {
        rc = text_reply(fd, RESP_ERROR, "an image with this name already exists in /data/homebrew");
    } else if (rename(staged, final) != 0) {
        rc = text_reply(fd, RESP_ERROR, "image could not be moved into /data/homebrew");
    } else {
        snprintf(message, sizeof(message), "OK {\"path\":\"%s\",\"size\":%llu}", final, (unsigned long long)part.st_size);
        rc = text_reply(fd, RESP_OK, message); published = true;
    }
    pthread_mutex_unlock(&g_transfer_lock);
    if (published) notify("SSPI image ready for ShadowMount");
    return rc;
}
static void install_json_string(const char *source, char *target, size_t capacity) {
    size_t n=0;
    for (; *source && n+7<capacity; ++source) {
        unsigned char c=(unsigned char)*source;
        if (c=='"' || c=='\\') { target[n++]='\\'; target[n++]=(char)c; }
        else if (c<32) n+=(size_t)snprintf(target+n,capacity-n,"\\u%04x",c);
        else target[n++]=(char)c;
    }
    target[n]=0;
}
static int helper_install_reply(int fd, const SspiInstallSnapshot *s, bool submission) {
    bool accepted=s->submission==SSPI_SUBMIT_ACCEPTED;
    bool rejected=s->outcome==SSPI_INSTALL_REJECTED;
    bool before_call=s->outcome==SSPI_INSTALL_FAILED && s->submission==SSPI_SUBMIT_NONE;
    const char *state;
    if (submission) state=accepted ? "submitted" : (rejected || before_call ? "failed" : "unconfirmed");
    else state=s->outcome==SSPI_INSTALL_COMPLETE ? "complete" :
        (s->outcome==SSPI_INSTALL_REJECTED || s->outcome==SSPI_INSTALL_FAILED ? "failed" :
        (s->outcome==SSPI_INSTALL_UNCONFIRMED || !accepted ? "unconfirmed" : "installing"));
    int code=submission && accepted ? 0 : s->api_code;
    if (!strcmp(state,"unconfirmed") && !code) code=-EINPROGRESS;
    char error[3100], native_status[100], response[6400];
    install_json_string(s->error,error,sizeof(error));
    install_json_string(s->status,native_status,sizeof(native_status));
    snprintf(response,sizeof(response),"{\"api_code\":%d,\"install_api_code\":%d,\"status_api_code\":%d,\"auth_restore_code\":%d,\"terminate_code\":%d,\"state\":\"%s\",\"content_id\":\"%s\",\"path\":\"%s\",\"attempt_id\":\"%s\",\"helper_pid\":%d,\"phase\":\"%s\",\"submission_attempted\":%s,\"submission_accepted\":%s,\"status\":\"%s\",\"progress\":%u,\"downloaded\":%llu,\"total\":%llu,\"error_code\":%d,\"error\":\"%s\"}",
        code,s->install_code,s->status_code,s->auth_restore_code,s->terminate_code,state,s->content_id,s->path,s->attempt,
        s->helper_pid,s->phase,before_call ? "false" : "true",accepted ? "true" : "false",native_status,s->progress,
        (unsigned long long)s->downloaded,(unsigned long long)s->total,s->error_code,error);
    size_t used=strlen(response);
    if (used && used+100<sizeof(response)) snprintf(response+used-1,sizeof(response)-used+1,",\"helper_running\":%s,\"receiver_recovery_required\":%s}",s->helper_running ? "true" : "false",s->outcome==SSPI_INSTALL_UNCONFIRMED && !s->helper_running && !s->ownership_released ? "true" : "false");
    return text_reply(fd,submission && accepted ? RESP_OK : code || rejected || before_call || s->outcome==SSPI_INSTALL_FAILED ? RESP_ERROR : submission ? RESP_OK : RESP_DATA,response);
}
static int handle_preflight(int fd) {
    /* The fresh installer validates its own service environment. Loading AppInst
       in this long-lived receiver cannot establish that child's readiness. */
    pthread_mutex_lock(&g_install_lock);
    sspi_install_poll(); bool busy=sspi_install_busy(),running=sspi_install_running();
    pthread_mutex_unlock(&g_install_lock);
    if (busy) return text_reply(fd,RESP_ERROR,running ? "An installation is still active; wait for it to finish" : "Previous install result is unconfirmed. Verify its installed version or reload receiver in Tools > Payloads, then Retry.");
    if (atomic_load(&g_receiver_appinst)) return text_reply(fd,RESP_ERROR,"A dump registration is still waiting for the console's installer service. Wait for it, or reload receiver in Tools > Payloads, then Retry.");
    unsigned long long freeb=0;
    struct statfs fs;
    if (statfs("/data",&fs)==0) freeb=(unsigned long long)fs.f_bavail*(unsigned long long)fs.f_bsize;
    char message[192];
    snprintf(message,sizeof(message),"OK {\"authid\":\"deferred\",\"appinst\":\"fresh-process\",\"free\":%llu}",freeb);
    return text_reply(fd,RESP_OK,message);
}
static int handle_install(int fd, const char *path) {
    if (!allowed_path(path) || !sspi_install_package_path(path)) return install_error_reply(fd,-1,"invalid PKG path",path);
    pthread_mutex_lock(&g_transfer_lock);
    Transfer *verified=find_transfer(path);
    struct stat st;
    if (!verified || !verified->verified || verified->active_lanes || fstat(verified->fd,&st) || !S_ISREG(st.st_mode) || !valid_package_fd(verified->fd,(uint64_t)st.st_size)) {
        pthread_mutex_unlock(&g_transfer_lock);
        return install_error_reply(fd,-1,"PKG was not verified",path);
    }
    pthread_mutex_lock(&g_install_lock);
    if (atomic_load(&g_stopping) || atomic_load(&g_stop_pending)) {
        pthread_mutex_unlock(&g_install_lock); pthread_mutex_unlock(&g_transfer_lock);
        return install_error_reply(fd,-ECANCELED,"Receiver is stopping",path);
    }
    if (atomic_load(&g_receiver_appinst)) {
        pthread_mutex_unlock(&g_install_lock); pthread_mutex_unlock(&g_transfer_lock);
        return install_error_reply(fd,-EBUSY,"A dump registration is using the console installer; wait for it or reload the receiver",path);
    }
    SspiInstallSnapshot result={0};
    int rc=sspi_install_start(path,g_delivery_title,g_delivery_icon,verified->fd);
    if (!rc) { result=*sspi_install_snapshot(); g_install_success_notified=false; g_install_failure_notified=false; }
    pthread_mutex_unlock(&g_install_lock);
    pthread_mutex_unlock(&g_transfer_lock);
    if (rc) return install_error_reply(fd,rc,rc==-EBUSY ? "An earlier installation still needs status verification" : "Installer launch preparation failed",path);
    /* Wait briefly for the first result without holding transfer, operation or
       install locks. Slow launches return a correlated pending status. */
    uint64_t until=sspi_install_millis()+3000;
    for (;;) {
        pthread_mutex_lock(&g_install_lock);
        sspi_install_poll(); const SspiInstallSnapshot *latest=sspi_install_snapshot();
        bool same=latest && !strcmp(latest->attempt,result.attempt);
        if (same) result=*latest;
        pthread_mutex_unlock(&g_install_lock);
        if (!same) {
            result.outcome=SSPI_INSTALL_UNCONFIRMED; result.api_code=-ECHILD; result.helper_running=0;
            snprintf(result.error,sizeof(result.error),"Another request replaced this finished attempt before its result was acknowledged");
            break;
        }
        if (result.submission==SSPI_SUBMIT_ACCEPTED || result.outcome!=SSPI_INSTALL_RUNNING || sspi_install_millis()>=until) break;
        usleep(10000);
    }
    if (result.submission==SSPI_SUBMIT_ACCEPTED) {
        pthread_mutex_lock(&g_transfer_lock);
        verified=find_transfer(path);
        if (verified && !verified->active_lanes) destroy_transfer_locked(verified);
        pthread_mutex_unlock(&g_transfer_lock);
    }
    int response=helper_install_reply(fd,&result,true);
    if (result.submission==SSPI_SUBMIT_ACCEPTED) notify("SSPI install submitted");
    else if (result.outcome==SSPI_INSTALL_REJECTED || result.outcome==SSPI_INSTALL_FAILED) notify("SSPI install submission failed");
    return response;
}
static int handle_status(int fd, const char *id) {
    pthread_mutex_lock(&g_install_lock);
    sspi_install_poll();
    const SspiInstallSnapshot *helper=sspi_install_snapshot();
    bool helper_matches=helper && ((!id || !*id) || !strcmp(id,helper->content_id));
    if (helper_matches) {
        bool success=helper->outcome==SSPI_INSTALL_COMPLETE && !g_install_success_notified;
        bool failed=(helper->outcome==SSPI_INSTALL_FAILED || helper->outcome==SSPI_INSTALL_REJECTED) && !g_install_failure_notified;
        g_install_success_notified|=success; g_install_failure_notified|=failed;
        SspiInstallSnapshot copy=*helper;
        pthread_mutex_unlock(&g_install_lock);
        int result=helper_install_reply(fd,&copy,false);
        if (success) notify("SSPI install complete");
        if (failed) notify("SSPI install failed");
        return result;
    }
    pthread_mutex_unlock(&g_install_lock);
    return text_reply(fd,RESP_ERROR,"{\"api_code\":-1,\"status_api_code\":-1,\"state\":\"unconfirmed\",\"error\":\"No current installer owns this content ID; verify the installed library\"}");
}

static int handle_install_confirm(int fd,const uint8_t *body,uint32_t size) {
    if (!body || size<6 || size>SSPI_INSTALL_PATH+48+32) return text_reply(fd,RESP_ERROR,"Invalid install confirmation");
    const char *values[3]; const uint8_t *cursor=body,*end=body+size;
    for (unsigned i=0;i<3;++i) {
        const uint8_t *zero=memchr(cursor,0,(size_t)(end-cursor));
        if (!zero || zero==cursor) return text_reply(fd,RESP_ERROR,"Invalid install confirmation");
        values[i]=(const char *)cursor; cursor=zero+1;
    }
    if (cursor!=end) return text_reply(fd,RESP_ERROR,"Invalid install confirmation");
    pthread_mutex_lock(&g_install_lock);
    int rc=sspi_install_confirm(values[0],values[1],values[2]);
    pthread_mutex_unlock(&g_install_lock);
    return text_reply(fd,rc ? RESP_ERROR : RESP_OK,rc==-EBUSY ? "Installer is still running; wait for it to exit" : rc ? "Install confirmation does not match a reaped unconfirmed attempt; reload receiver if necessary" : "OK");
}

static int handle_title_context(int fd, const uint8_t *body, uint32_t size) {
    if (!body || size < 12) return text_reply(fd, RESP_ERROR, "invalid title context");
    const char *id = (const char *)body;
    const uint8_t *id_end = memchr(body, 0, size);
    if (!id_end || (size_t)(id_end - body) != 9 || !is_tid(id)) return text_reply(fd, RESP_ERROR, "invalid title id");
    const uint8_t *name = id_end + 1;
    size_t remaining = size - (size_t)(name - body);
    const uint8_t *name_end = memchr(name, 0, remaining);
    if (!name_end || name_end == name || name_end - name >= 220) return text_reply(fd, RESP_ERROR, "invalid title name");
    const uint8_t *icon = name_end + 1;
    remaining = size - (size_t)(icon - body);
    const uint8_t *icon_end = memchr(icon, 0, remaining);
    if (!icon_end || icon_end != body + size - 1 || icon_end - icon >= 1000) return text_reply(fd, RESP_ERROR, "invalid icon URI");
    if (*icon && strncmp((const char *)icon, "https://", 8) && strncmp((const char *)icon, "http://", 7)) return text_reply(fd, RESP_ERROR, "icon URI must be HTTP(S)");
    pthread_mutex_lock(&g_install_lock);
    sspi_install_poll();
    if (g_hold_system_authid || sspi_install_busy()) { pthread_mutex_unlock(&g_install_lock); return text_reply(fd, RESP_ERROR, "installation is active"); }
    pthread_mutex_lock(&g_notify_lock);
    snprintf(g_delivery_title, sizeof(g_delivery_title), "%s [%s]", (const char *)name, id);
    snprintf(g_delivery_icon, sizeof(g_delivery_icon), "%s", (const char *)icon);
    g_last_incoming[0] = 0;
    pthread_mutex_unlock(&g_notify_lock);
    pthread_mutex_unlock(&g_install_lock);
    return text_reply(fd, RESP_OK, "OK");
}

/* Native notification JSON follows the public payload SDK notification ABI.
 * Progress is never logged in Notification Center; only completion is retained. */
static int handle_progress_notification(int fd, const uint8_t *body, uint32_t size) {
    if (!body || size < 18 || body[0] > 1 || body[10] != 0 || !is_tid((const char *)body + 1))
        return text_reply(fd, RESP_ERROR, "invalid notification identity");
    uint32_t image_size = read_u32le(body + 11);
    if (image_size > 512u * 1024u || image_size > size - 17)
        return text_reply(fd, RESP_ERROR, "invalid notification artwork size");
    const uint8_t *image = body + 15;
    const char *json = (const char *)(image + image_size);
    size_t json_size = size - 15 - image_size;
    if (json_size > 16384 || json[0] != '{' || json[json_size - 1] != 0 || memchr(json, 0, json_size - 1))
        return text_reply(fd, RESP_ERROR, "invalid notification payload");
    if (image_size) {
        bool png = image_size >= 8 && !memcmp(image, "\x89PNG\r\n\x1a\n", 8);
        bool jpeg = image_size >= 3 && image[0] == 0xff && image[1] == 0xd8 && image[2] == 0xff;
        if (!png && !jpeg) return text_reply(fd, RESP_ERROR, "unsupported notification artwork");
        mkdir("/data/SSPI/artwork", 0775);
        char path[96], temporary[104];
        snprintf(path, sizeof(path), "/data/SSPI/artwork/%.9s.png", (const char *)body + 1);
        snprintf(temporary, sizeof(temporary), "%s.next", path);
        pthread_mutex_lock(&g_notify_lock);
        int output = open(temporary, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0664);
        size_t written = 0;
        if (output >= 0) {
            while (written < image_size) {
                ssize_t count = write(output, image + written, image_size - written);
                if (count < 0 && errno == EINTR) continue;
                if (count <= 0) break;
                written += (size_t)count;
            }
            close(output);
        }
        int saved = written == image_size ? rename(temporary, path) : -1;
        if (saved) unlink(temporary);
        pthread_mutex_unlock(&g_notify_lock);
        if (saved) return text_reply(fd, RESP_ERROR, "could not cache notification artwork");
    }
    time_t now = time(NULL);
    struct tm utc;
    char timestamp[40], dated_payload[16500];
    if (!gmtime_r(&now, &utc) || !strftime(timestamp, sizeof(timestamp), "%Y-%m-%dT%H:%M:%S.000Z", &utc))
        return text_reply(fd, RESP_ERROR, "notification timestamp unavailable");
    snprintf(dated_payload, sizeof(dated_payload), "{\"createdDateTime\":\"%s\",%s", timestamp, json + 1);
    int rc = sceNotificationSend(0xfe, body[0] == 1, dated_payload);
    if (rc) { char error[80]; snprintf(error, sizeof(error), "notification API returned %d", rc); return text_reply(fd, RESP_ERROR, error); }
    return text_reply(fd, RESP_OK, "OK");
}

static int handle_console_tools(int fd, unsigned cmd, const uint8_t *b, uint32_t n) {
    if (cmd==CMD_LIST_INSTALLED || cmd==CMD_SYSTEM_INFO || cmd==CMD_SHELL_REFRESH) {
        if (n) return text_reply(fd,RESP_ERROR,"This command requires an empty request.");
        if (cmd==CMD_SHELL_REFRESH) return text_reply(fd,RESP_ERROR,"Restart your PS5 to see the new icons.");
        size_t cap=cmd==CMD_LIST_INSTALLED ? PS5_LIBRARY_CAP : PS5_SYSTEM_INFO_CAP;
        char *out=malloc(cap); if (!out) return text_reply(fd,RESP_ERROR,"Not enough memory to read console information.");
        int rc=cmd==CMD_LIST_INSTALLED ? ps5_library_json(out,cap) : ps5_system_info(out,cap);
        int result=rc ? text_reply(fd,RESP_ERROR,"Console information could not be read.") : text_reply(fd,RESP_DATA,out);
        free(out); return result;
    }
    if (!b || n<10 || b[9] || memchr(b,0,9) || !ct_valid_id((const char *)b)) return text_reply(fd,RESP_ERROR,"Invalid installed title ID.");
    if (((cmd==CMD_INSTALLED_METADATA || cmd==CMD_TITLE_ICON_RESTORE) && n!=10) ||
        (cmd==CMD_TITLE_ICON_GET && (n!=11 || b[10]>1)) ||
        (cmd==CMD_TITLE_ICON_SET && (n<=10 || n>CT_MAX_PNG+10))) return text_reply(fd,RESP_ERROR,"Invalid icon or metadata request length.");
    if (!ps5_title_installed((const char *)b)) return text_reply(fd,RESP_ERROR,"Refresh Library to confirm this title is installed.");
    if (cmd==CMD_INSTALLED_METADATA || cmd==CMD_TITLE_ICON_GET) {
        uint8_t *out=malloc(cmd==CMD_INSTALLED_METADATA ? CT_METADATA_SIZE : CT_MAX_PNG); size_t size=0;
        if (!out) return text_reply(fd,RESP_ERROR,"Not enough memory to read the icon.");
        int rc=cmd==CMD_INSTALLED_METADATA ? ps5_metadata((const char *)b,out,&size) : ct_icon_get("/data/SSPI",(const char *)b,b[10]!=0,out,&size);
        int result=rc ? text_reply(fd,RESP_ERROR,"The title metadata or icon is unavailable.") : reply(fd,RESP_DATA,out,(uint32_t)size);
        free(out); return result;
    }
    char out[512]; int rc=ct_icon_change("/data/SSPI","PS5",(const char *)b,b+10,n-10,cmd==CMD_TITLE_ICON_RESTORE,out,sizeof(out));
    return text_reply(fd,rc?RESP_ERROR:RESP_DATA,out);
}
/* Stops an app or a payload loaded through elfldr; everything else is refused. */
static int handle_process_control(int fd, const uint8_t *b, uint32_t n) {
    char out[512], error[200]="The process could not be stopped.";
    return ps5_process_control(b,n,out,sizeof(out),error,sizeof(error)) ? text_reply(fd,RESP_ERROR,error) : text_reply(fd,RESP_DATA,out);
}
/* Read-only diagnostics: kernel log, processes, payload logs and crash reports. */
static int handle_diagnostics(int fd, unsigned cmd, const uint8_t *b, uint32_t n) {
    if (cmd!=DX_CMD_LOG_READ && n) return text_reply(fd,RESP_ERROR,"This command requires an empty request.");
    size_t cap=cmd==DX_CMD_PROCESSES ? PS5_PROCESSES_CAP : cmd==DX_CMD_LOG_LIST ? DX_LIST_MAX : DX_REPLY_MAX;
    char *out=malloc(cap), error[160]="Diagnostics could not be read.";
    if (!out) return text_reply(fd,RESP_ERROR,"Not enough memory to read diagnostics.");
    size_t size=0; int rc;
    if (cmd==DX_CMD_PROCESSES) { rc=ps5_processes_json(out,cap); if (!rc) size=strlen(out); else snprintf(error,sizeof(error),"The process list could not be read."); }
    else if (cmd==DX_CMD_KERNEL_LOG) rc=dx_kernel_log(out,cap,&size,error,sizeof(error));
    else if (cmd==DX_CMD_LOG_LIST) rc=dx_log_list(out,cap,&size,error,sizeof(error));
    else rc=dx_log_read(b,n,out,cap,&size,error,sizeof(error));
    int result=rc ? text_reply(fd,RESP_ERROR,error) : reply(fd,RESP_DATA,out,(uint32_t)size);
    free(out); return result;
}
static int handle_stop(int fd, uint32_t size) {
    if (size) return text_reply(fd,RESP_ERROR,"STOP requires an empty request.");
    pthread_mutex_lock(&g_transfer_lock);
    bool busy=g_upload_lanes!=0;
    pthread_mutex_lock(&g_install_lock);
    sspi_install_poll();
    busy |= sspi_install_running();
    if (!busy) atomic_store(&g_stop_pending,true);
    pthread_mutex_unlock(&g_install_lock); pthread_mutex_unlock(&g_transfer_lock);
    if (busy) return text_reply(fd,RESP_ERROR,"A transfer or installation is active. Wait for it to finish before stopping the receiver.");
    trace_mark("stop requested", "idle");
    int rc=text_reply(fd,RESP_OK,"stopping"); atomic_store(&g_stopping,true); return rc?rc:-1;
}
static void *client_thread(void *argument) {
    unsigned slot=(unsigned)(uintptr_t)argument; int fd=g_client_fds[slot]; Lane lane; memset(&lane, 0, sizeof(lane));
    uint8_t *upload_buffer = NULL;
    while (!atomic_load(&g_stopping)) {
        uint8_t header[5];
        if (recv_all(fd, header, sizeof(header))) break;
        uint32_t size = read_u32le(header + 1);
        if (size > MAX_FRAME) { text_reply(fd, RESP_ERROR, "frame too large"); break; }
        if (header[0] == CMD_UPLOAD_CHUNK) {
            if (!upload_buffer) upload_buffer = malloc(UPLOAD_BUFFER);
            if (!upload_buffer) { text_reply(fd, RESP_ERROR, "out of memory"); trace_resources("upload allocation failed"); break; }
            if (handle_upload_chunk(fd, size, &lane, upload_buffer)) break;
            continue;
        }
        uint32_t control_limit = header[0]==CMD_PROGRESS_NOTIFICATION ? 512u*1024u+16400u :
            header[0]==CMD_TITLE_ICON_SET ? CT_MAX_PNG+10 : MAX_PATH_BYTES+25;
        if (size > control_limit) { text_reply(fd, RESP_ERROR, "control frame too large"); break; }
        uint8_t *body = size ? malloc(size + 1) : NULL;
        if (size && (!body || recv_all(fd, body, size))) { free(body); break; }
        if (body) body[size] = 0;
        int result = 0;
        /* Dump mounts serialize on their own lock: a stalled AppInst call must not
           hold uploads or STOP, which releases the port for a fresh receiver. */
        bool guarded=header[0]==CMD_START_UPLOAD || header[0]==CMD_IMAGE ||
            header[0]==CMD_STOP || (header[0]>=CMD_TITLE_ICON_GET && header[0]<=CMD_TITLE_ICON_RESTORE);
        if (guarded) pthread_mutex_lock(&g_operation_lock);
        if (atomic_load(&g_stopping)) { if (guarded) pthread_mutex_unlock(&g_operation_lock); free(body); break; }
        switch (header[0]) {
            case CMD_STOP: result=handle_stop(fd,size); break;
            case CMD_LIST_INSTALLED: case CMD_INSTALLED_METADATA: case CMD_TITLE_ICON_GET:
            case CMD_TITLE_ICON_SET: case CMD_TITLE_ICON_RESTORE: case CMD_SHELL_REFRESH: case CMD_SYSTEM_INFO:
                result=handle_console_tools(fd,header[0],body,size); break;
            case DX_CMD_KERNEL_LOG: case DX_CMD_PROCESSES: case DX_CMD_LOG_LIST: case DX_CMD_LOG_READ:
                result=handle_diagnostics(fd,header[0],body,size); break;
            case CMD_PING: result = text_reply(fd, RESP_OK, "SSPI"); break;
            case CMD_CREATE_DIR: result = allowed_path((char *)body) && mkdir_parents((char *)body) == 0 && (mkdir((char *)body, 0775) == 0 || errno == EEXIST) ? text_reply(fd, RESP_OK, "OK") : text_reply(fd, RESP_ERROR, "directory rejected"); break;
            case CMD_START_UPLOAD: result = handle_start(fd, body, size, &lane); break;
            case CMD_END_UPLOAD: result = size ? text_reply(fd, RESP_ERROR, "END must be empty") : handle_end(fd, &lane); break;
            case CMD_VERIFY_FILE: result = handle_verify(fd, (char *)body); break;
            case CMD_IMAGE: result = handle_image(fd, body, size); break;
            case PC_CMD_CONTROL: result = handle_process_control(fd, body, size); break;
            case CMD_TITLE_CONTEXT: result = handle_title_context(fd, body, size); break;
            case CMD_PROGRESS_NOTIFICATION: result = handle_progress_notification(fd, body, size); break;
            case CMD_INSTALL_PREFLIGHT: result = handle_preflight(fd); break;
            case CMD_INSTALL_PKG: result = handle_install(fd, (char *)body); break;
            case CMD_CONFIRM_INSTALL: result = handle_install_confirm(fd, body, size); break;
            case CMD_INSTALL_STATUS: result = handle_status(fd, (char *)body); break;
            case CMD_MOUNT_GAME: result = handle_mount_game(fd, (char *)body); break;
            case CMD_GET_CONFIG: { const char *appinst = "untried", *authid = "untried"; char appinst_error[32], authid_error[32]; bool init_tried = atomic_load(&g_appinst_init_attempted), auth_tried = atomic_load(&g_authid_attempted); int init_rc = atomic_load(&g_appinst_init_rc), auth_rc = atomic_load(&g_authid_rc); if (atomic_load(&g_receiver_appinst)) appinst = "busy"; else if (init_tried) { if (init_rc) { snprintf(appinst_error, sizeof(appinst_error), "unavailable:%d", init_rc); appinst = appinst_error; } else appinst = "ready"; } if (auth_tried) { if (auth_rc) { snprintf(authid_error, sizeof(authid_error), "unavailable:%d", auth_rc); authid = authid_error; } else authid = "system-install"; } char config[640]; snprintf(config, sizeof(config), "{\"port\":%d,\"version\":\"%s\",\"platform\":\"ps5\",\"authid\":\"%s\",\"appinst\":\"%s\",\"capabilities\":[" PS5_INSTALL_CAPS "]}", g_port, VERSION, authid, appinst); result = text_reply(fd, RESP_DATA, config); break; }
            case CMD_SET_PORT: { char *end = NULL; long port = body ? strtol((char *)body, &end, 10) : 0; result = end && *end == 0 && port >= 1024 && port <= 65535 && config_save((int)port) == 0 ? text_reply(fd, RESP_OK, "OK restart required") : text_reply(fd, RESP_ERROR, "invalid port or config write failed"); break; }
            default: result = text_reply(fd, RESP_ERROR, "unsupported command"); break;
        }
        if (guarded) pthread_mutex_unlock(&g_operation_lock);
        free(body); if (result) break;
    }
    bool failed_lane = lane.transfer != NULL && !lane.ended;
    release_lane(&lane);
    free(upload_buffer);
    /* The listener joins a thread once its descriptor is cleared; finish locked
       and file work first so that join never waits for the transfer lock. */
    if (failed_lane) trace_resources("lane disconnected");
    pthread_mutex_lock(&g_clients_lock); close(fd); g_client_fds[slot]=-1; pthread_mutex_unlock(&g_clients_lock);
    atomic_fetch_sub(&g_clients, 1);
    return NULL;
}

/* Listener diagnostics never wait for a worker's transfer lock, and each kind of
   repeated failure is written at most once a minute with a count of the rest. */
typedef struct { bool logged; uint64_t at; unsigned repeats; } ListenerNote;
static void listener_note(ListenerNote *note, const char *event, int code) {
    uint64_t now = sspi_install_millis();
    if (note->logged && now - note->at < 60000) { note->repeats++; return; }
    char lanes[48] = "lanes=busy", detail[160];
    if (!pthread_mutex_trylock(&g_transfer_lock)) {
        snprintf(lanes, sizeof(lanes), "lanes=%u transfers=%u", g_upload_lanes, g_transfer_count);
        pthread_mutex_unlock(&g_transfer_lock);
    }
    snprintf(detail, sizeof(detail), "errno=%d repeats=%u clients=%u %s", code, note->repeats, atomic_load(&g_clients), lanes);
    trace_mark(event, detail);
    note->logged = true; note->at = now; note->repeats = 0;
}
static void maintain_install(void) {
    /* This runs on the accept thread. A worker may hold either lock while a
       native service or disk operation stalls; retry on the next listener tick. */
    if (pthread_mutex_trylock(&g_transfer_lock)) return;
    if (pthread_mutex_trylock(&g_install_lock)) {
        pthread_mutex_unlock(&g_transfer_lock);
        return;
    }
    sspi_install_poll();
    const SspiInstallSnapshot *s=sspi_install_snapshot();
    if (s && s->submission==SSPI_SUBMIT_ACCEPTED) {
        Transfer *transfer=find_transfer(s->path);
        if (transfer && transfer->verified && !transfer->active_lanes) destroy_transfer_locked(transfer);
    }
    pthread_mutex_unlock(&g_install_lock);
    pthread_mutex_unlock(&g_transfer_lock);
}
int main(void) {
    for (unsigned i=0;i<MAX_CLIENTS;i++) g_client_fds[i]=-1;
    signal(SIGPIPE, SIG_IGN);
    /* Name ourselves so process lists show sspi.elf instead of the loader's
       default "payload.elf" (elfldr only derives a name from file:// URIs). */
    syscall(SYS_thr_set_name, -1, "sspi.elf");
    sspi_startup_note("main storage init begin", 0);
    mkdir("/data/SSPI", 0775);
    trace_mark("boot", "sspi.elf " VERSION);
    sspi_startup_note("startup notification begin", 0);
    notify("SSPI payload started");
    sspi_startup_note("startup notification complete", 0);
    mkdir("/data/SSPI", 0775); mkdir("/user/data/tmp", 0775); mkdir("/data/homebrew", 0775); mkdir("/data/homebrew/backports", 0775); g_port = config_load();
    sspi_startup_note("listener port", g_port);
    int server = socket(AF_INET, SOCK_STREAM, 0); if (server < 0) { sspi_startup_note("socket failed", errno); notify("SSPI socket failed"); return 1; } int yes = 1, buffer = 1 * 1024 * 1024; setsockopt(server, SOL_SOCKET, SO_REUSEADDR, &yes, sizeof(yes)); setsockopt(server, SOL_SOCKET, SO_RCVBUF, &buffer, sizeof(buffer)); setsockopt(server, SOL_SOCKET, SO_SNDBUF, &buffer, sizeof(buffer)); setsockopt(server, IPPROTO_TCP, TCP_NODELAY, &yes, sizeof(yes));
    struct sockaddr_in address; memset(&address, 0, sizeof(address)); address.sin_family = AF_INET; address.sin_addr.s_addr = htonl(INADDR_ANY); address.sin_port = htons((uint16_t)g_port);     if (bind(server, (struct sockaddr *)&address, sizeof(address))) { sspi_startup_note("bind failed", errno); notify("SSPI bind failed"); close(server); return 2; } if (listen(server, 128)) { sspi_startup_note("listen failed", errno); notify("SSPI listen failed"); close(server); return 2; }
    sspi_startup_note("listener ready", g_port);
    char ready[80]; snprintf(ready, sizeof(ready), "SSPI receiver ready on %d", g_port); notify(ready);
    pthread_attr_t attributes;
    if (pthread_attr_init(&attributes) != 0) { close(server); return 3; }
    if (pthread_attr_setstacksize(&attributes, CLIENT_STACK) != 0) { pthread_attr_destroy(&attributes); close(server); return 3; }
    ListenerNote select_note = {0}, accept_note = {0}, capacity_note = {0}, thread_note = {0};
    while (!atomic_load(&g_stopping)) {
        maintain_install();
        for (unsigned i=0;i<MAX_CLIENTS;i++) {
            pthread_mutex_lock(&g_clients_lock); bool done=g_client_fds[i]<0; pthread_mutex_unlock(&g_clients_lock);
            if (g_client_used[i] && done && !pthread_join(g_client_threads[i],NULL)) g_client_used[i]=false;
        }
        fd_set ready_fds; FD_ZERO(&ready_fds); FD_SET(server,&ready_fds); struct timeval wait={0,100000};
        int available=select(server+1,&ready_fds,NULL,NULL,&wait);
        if (available<=0) {
            int code=errno;
            if (available<0 && code!=EINTR) {
                listener_note(&select_note,"listener select failed",code);
                if (code!=EAGAIN && code!=ENOMEM) { trace_mark("listener stopped","select failed"); break; }
                usleep(100000);
            }
            continue;
        }
        int accepted = accept(server, NULL, NULL);
        if (accepted < 0) {
            int code = errno;
            if (code != EINTR) { listener_note(&accept_note, "accept failed", code); usleep(100000); }
            continue;
        }
        if (atomic_fetch_add(&g_clients, 1) >= MAX_CLIENTS) {
            atomic_fetch_sub(&g_clients, 1);
            close(accepted);
            listener_note(&capacity_note, "client limit", 0);
            continue;
        }
        unsigned slot=0; while (slot<MAX_CLIENTS && g_client_used[slot]) slot++;
        if (slot==MAX_CLIENTS) { close(accepted); atomic_fetch_sub(&g_clients,1); listener_note(&capacity_note, "client slots unjoined", 0); continue; }
        g_client_fds[slot]=accepted; int *client=&g_client_fds[slot];
        struct timeval receive_timeout = {120, 0}, send_timeout = {30, 0};
        setsockopt(*client, SOL_SOCKET, SO_RCVTIMEO, &receive_timeout, sizeof(receive_timeout));
        setsockopt(*client, SOL_SOCKET, SO_SNDTIMEO, &send_timeout, sizeof(send_timeout));
        setsockopt(*client, SOL_SOCKET, SO_RCVBUF, &buffer, sizeof(buffer));
        setsockopt(*client, SOL_SOCKET, SO_SNDBUF, &buffer, sizeof(buffer));
        setsockopt(*client, IPPROTO_TCP, TCP_NODELAY, &yes, sizeof(yes));
#ifdef SO_NOSIGPIPE
        setsockopt(*client, SOL_SOCKET, SO_NOSIGPIPE, &yes, sizeof(yes));
#endif
        int created = pthread_create(&g_client_threads[slot], &attributes, client_thread, (void *)(uintptr_t)slot);
        if (!created) g_client_used[slot]=true;
        else { close(*client); *client=-1; atomic_fetch_sub(&g_clients, 1); listener_note(&thread_note, "thread creation failed", created); }
    }
    atomic_store(&g_stopping,true); close(server); pthread_attr_destroy(&attributes);
    pthread_mutex_lock(&g_clients_lock);
    for (unsigned i=0;i<MAX_CLIENTS;i++) if (g_client_fds[i]>=0) shutdown(g_client_fds[i],SHUT_RDWR);
    pthread_mutex_unlock(&g_clients_lock);
    for (unsigned i=0;i<MAX_CLIENTS;i++) if (g_client_used[i]) pthread_join(g_client_threads[i],NULL);
    pthread_mutex_lock(&g_transfer_lock);
    while (g_transfers) destroy_transfer_locked(g_transfers);
    pthread_mutex_unlock(&g_transfer_lock);
    trace_mark("stop","receiver threads stopped"); return 0;
}
