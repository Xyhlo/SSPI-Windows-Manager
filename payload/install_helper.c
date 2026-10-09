#ifdef SSPI_INSTALL_TEST
#include "install-test-platform.h"
#else
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <ps5/kernel.h>
#endif
#include "install_protocol.h"

typedef struct { char content_id[48]; int content_type, content_platform; } PackageInfo;
typedef struct { const char *uri, *ex_uri, *scenario, *content_id, *name, *icon; } Metadata;
typedef struct { char languages[30][8], scenarios[64][3], content_ids[64][48]; int64_t unknown[810]; } PlayGo;
typedef struct { int32_t code, version; char description[512], type[9]; } InstallError;
typedef struct { char status[16], src_type[8]; uint32_t remain; uint64_t downloaded, initial, total; uint32_t promote; InstallError error; int32_t local_percent; bool copy_only; } InstallStatus;
_Static_assert(sizeof(PackageInfo)==0x38 && sizeof(Metadata)==0x30 && sizeof(PlayGo)==0x2700 && sizeof(InstallStatus)==0x258, "AppInst ABI");

/* AppInst can retain every argument until Terminate. Never use request-stack data. */
static SspiInstallRequest request;
static SspiInstallSnapshot snapshot;
static Metadata metadata;
static PackageInfo package;
static PlayGo playgo;
static char directory[128];
static uint64_t original_authid;
static bool initialized;
static int (*app_init)(void), (*app_term)(void);
static int (*app_install)(Metadata *, PackageInfo *, PlayGo *);
static int (*app_status)(const char *, InstallStatus *);

static int publish(const char *phase) {
    if (phase) snprintf(snapshot.phase, sizeof(snapshot.phase), "%s", phase);
    ++snapshot.sequence;
    return sspi_install_write(directory, "status", &snapshot, sizeof(snapshot));
}
static int finish(unsigned outcome, int code, const char *message) {
    snapshot.outcome = outcome;
    snapshot.api_code = code;
    if (message && message != snapshot.error) snprintf(snapshot.error, sizeof(snapshot.error), "%s", message);
    /* Keep SYSTEM AuthID and argument storage valid through installer shutdown. */
    if (initialized) {
        publish("terminating");
        snapshot.terminate_code = app_term();
        initialized = false;
    }
    if (original_authid && original_authid != SSPI_INSTALL_AUTHID) {
        for (unsigned i=0; i<3; ++i) {
            kernel_set_ucred_authid(getpid(), original_authid);
            if (kernel_get_ucred_authid(getpid()) == original_authid) break;
        }
        if (kernel_get_ucred_authid(getpid()) != original_authid) snapshot.auth_restore_code = -EPERM;
    }
    publish("finished");
    return outcome == SSPI_INSTALL_COMPLETE ? 0 : 1;
}
static void *load_service(const char *name) {
    dlerror();
    void *module=dlopen(name,RTLD_NOW|RTLD_LOCAL);
    if (!module) {
        const char *detail=dlerror();
        snprintf(snapshot.error,sizeof(snapshot.error),"%s could not be loaded: %.350s",name,detail ? detail : "no loader detail");
    }
    return module;
}
static void *service_symbol(void *module,const char *name) {
    dlerror();
    void *symbol=dlsym(module,name);
    if (!symbol) {
        const char *detail=dlerror();
        snprintf(snapshot.error,sizeof(snapshot.error),"%s is unavailable: %.350s",name,detail ? detail : "no loader detail");
    }
    return symbol;
}
static bool load_appinst(void) {
    if (publish("authid")) return false;
    original_authid = kernel_get_ucred_authid(getpid());
    if (!original_authid || kernel_set_ucred_authid(getpid(), SSPI_INSTALL_AUTHID) || kernel_get_ucred_authid(getpid()) != SSPI_INSTALL_AUTHID) {
        snprintf(snapshot.error, sizeof(snapshot.error), "System install AuthID preparation failed");
        return false;
    }
    /* SDK order matters: AppInstUtil can stop the process if Ipmi is absent. */
    if (publish("ipmi_load")) return false;
    if (!load_service("libSceIpmi.sprx")) return false;
    if (publish("appinst_load")) return false;
    void *module = load_service("libSceAppInstUtil.sprx");
    if (!module) return false;
    if (!(app_init=service_symbol(module,"sceAppInstUtilInitialize")) ||
        !(app_term=service_symbol(module,"sceAppInstUtilTerminate")) ||
        !(app_install=service_symbol(module,"sceAppInstUtilInstallByPackage")) ||
        !(app_status=service_symbol(module,"sceAppInstUtilGetInstallStatus"))) return false;
    if (publish("initializing")) return false;
    snapshot.api_code = app_init();
    initialized = snapshot.api_code == 0;
    if (!initialized) snprintf(snapshot.error, sizeof(snapshot.error), "AppInst initialization failed (0x%08x)", (unsigned)snapshot.api_code);
    return initialized;
}
static int install_one(void) {
    struct stat st;
    int file = open(request.path, O_RDONLY | O_NOFOLLOW);
    bool valid = file >= 0 && !fstat(file, &st) && S_ISREG(st.st_mode) && (uint64_t)st.st_size == request.file_size &&
        (uint64_t)st.st_ino == request.file_inode && sspi_install_package(file,request.file_size,NULL);
    if (file >= 0) close(file);
    if (!valid) return finish(SSPI_INSTALL_FAILED, -EINVAL, "The verified package changed before submission");
    if (!load_appinst()) return finish(SSPI_INSTALL_FAILED, snapshot.api_code ? snapshot.api_code : -ENOSYS, snapshot.error);
    metadata = (Metadata){request.path, "", "", "", request.title[0] ? request.title : "SSPI", request.icon};
    snapshot.submission = SSPI_SUBMIT_CALLING;
    if (publish("submitting")) {
        snapshot.submission = SSPI_SUBMIT_NONE;
        return finish(SSPI_INSTALL_FAILED, -EIO, "The install attempt could not be recorded");
    }
    /* Exactly one submission, including after a refusal. Retry creates another ELF process. */
    snapshot.install_code = app_install(&metadata, &package, &playgo);
    package.content_id[sizeof(package.content_id)-1] = 0;
    if (snapshot.install_code) {
        snapshot.submission = SSPI_SUBMIT_REJECTED;
        return finish(SSPI_INSTALL_REJECTED, snapshot.install_code, "AppInst rejected the package; Retry starts a fresh installer");
    }
    snapshot.submission = SSPI_SUBMIT_ACCEPTED;
    if (!sspi_install_content_id(package.content_id)) return finish(SSPI_INSTALL_UNCONFIRMED, -EPROTO, "The package was accepted without a valid content ID");
    snprintf(snapshot.content_id, sizeof(snapshot.content_id), "%s", package.content_id);
    publish("submitted");
    uint64_t last_progress = sspi_install_millis();
    uint64_t max_downloaded=0;
    unsigned max_progress=0,seen_count=0;
    char seen_status[8][16]={{0}};
    unsigned errors = 0;
    for (;;) {
        if (kill(request.parent, 0) && errno == ESRCH)
            return finish(SSPI_INSTALL_UNCONFIRMED, -ECHILD, "The receiver exited while installation was active");
        static struct { InstallStatus value; unsigned char spare[4096-sizeof(InstallStatus)], guard[32]; } buffer;
        memset(&buffer, 0, sizeof(buffer)); memset(buffer.guard, 0xa5, sizeof(buffer.guard));
        snapshot.status_code = app_status(snapshot.content_id, &buffer.value);
        bool overflow = false;
        for (size_t i=0; i<sizeof(buffer.spare); ++i) overflow |= buffer.spare[i] != 0;
        for (size_t i=0; i<sizeof(buffer.guard); ++i) overflow |= buffer.guard[i] != 0xa5;
        if (overflow) return finish(SSPI_INSTALL_UNCONFIRMED, -EOVERFLOW, "Install status exceeded the supported ABI");
        InstallStatus *s = &buffer.value;
        s->status[sizeof(s->status)-1] = 0; s->error.description[sizeof(s->error.description)-1] = 0;
        if (snapshot.status_code) {
            if (++errors >= 20) return finish(SSPI_INSTALL_UNCONFIRMED, snapshot.status_code, "Install status stopped answering");
        } else {
            errors = 0;
            unsigned progress = s->promote;
            if (s->local_percent > 0 && (unsigned)s->local_percent > progress) progress = (unsigned)s->local_percent;
            if (s->total && s->downloaded <= s->total) { unsigned p = (unsigned)((long double)s->downloaded * 100 / s->total); if (p > progress) progress = p; }
            if (progress > 100) progress = 100;
            bool advanced=s->downloaded>max_downloaded || progress>max_progress,seen=false;
            for (unsigned i=0;i<seen_count;++i) seen|=!strcmp(s->status,seen_status[i]);
            if (!seen && seen_count<8) { snprintf(seen_status[seen_count++],16,"%s",s->status); advanced=true; }
            if (s->downloaded>max_downloaded) max_downloaded=s->downloaded;
            if (progress>max_progress) max_progress=progress;
            if (advanced) last_progress=sspi_install_millis();
            snapshot.progress = progress; snapshot.downloaded = s->downloaded; snapshot.total = s->total;
            snprintf(snapshot.status, sizeof(snapshot.status), "%s", s->status);
            if (!strcmp(snapshot.status,"completed")) snprintf(snapshot.status,sizeof(snapshot.status),"complete");
            snapshot.error_code = s->error.code;
            if (s->error.code) return finish(SSPI_INSTALL_FAILED, s->error.code, s->error.description);
            if (!strcmp(s->status,"installed") || !strcmp(s->status,"complete") || !strcmp(s->status,"completed") || (!strcmp(s->status,"playable") && progress == 100))
                return finish(SSPI_INSTALL_COMPLETE, 0, "");
        }
        if (sspi_install_millis() - last_progress >= 600000) return finish(SSPI_INSTALL_UNCONFIRMED, -ETIMEDOUT, "The install made no progress for 10 minutes");
        if (publish("installing")) return finish(SSPI_INSTALL_UNCONFIRMED, -EIO, "The receiver could not be informed of install progress");
        usleep(500000);
    }
}
int main(int argc, char **argv) {
    signal(SIGPIPE, SIG_IGN);
    if (argc != 2 || !sspi_install_directory(argv[1])) return 2;
    snprintf(directory, sizeof(directory), "%s", argv[1]);
    if (sspi_install_read(directory, "request", &request, sizeof(request)) || request.magic != SSPI_INSTALL_MAGIC || request.size != sizeof(request) ||
        !memchr(request.attempt,0,sizeof(request.attempt)) || strcmp(request.attempt, directory+sizeof(SSPI_INSTALL_ROOT)-1) ||
        !memchr(request.path,0,sizeof(request.path)) || !sspi_install_package_path(request.path) ||
        !memchr(request.title,0,sizeof(request.title)) || !memchr(request.icon,0,sizeof(request.icon)) ||
        !memchr(request.content_id,0,sizeof(request.content_id)) || request.parent <= 0) return 2;
    snapshot.magic = SSPI_INSTALL_MAGIC; snapshot.size = sizeof(snapshot);
    snapshot.helper_pid = getpid();
    snprintf(snapshot.attempt, sizeof(snapshot.attempt), "%s", request.attempt);
    snprintf(snapshot.content_id, sizeof(snapshot.content_id), "%s", request.content_id);
    snprintf(snapshot.path, sizeof(snapshot.path), "%s", request.path);
    if (publish("starting")) return 2;
    return install_one();
}
