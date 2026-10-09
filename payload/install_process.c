#ifdef SSPI_INSTALL_TEST
#include "install-test-platform.h"
#else
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include "elfldr.h"
#include "install_loader.h"
#endif
#include "install_process.h"

extern const unsigned char sspi_install_elf[], sspi_install_elf_end[];
extern void sspi_install_trace(const char *stage, const char *detail);
static SspiInstallSnapshot current;
static SspiInstallRequest pending;
static char directory[128];
static pthread_t loader_thread;
static bool loader_joinable, owned, watchdog_failed;
static atomic_bool loading;
static atomic_int child_pid, load_result;
static void *child_stack;
static uint64_t load_deadline, last_update, stop_deadline, kill_after;
static unsigned kill_attempts;

int sspi_install_pid(void) { return atomic_load(&child_pid); }
const char *sspi_install_path(void) { return pending.path; }
const SspiInstallSnapshot *sspi_install_snapshot(void) { current.helper_running=sspi_install_running(); return owned ? &current : NULL; }
bool sspi_install_running(void) { return atomic_load(&loading) || sspi_install_pid() > 0; }
bool sspi_install_loading(void) { return atomic_load(&loading); }
bool sspi_install_busy(void) {
    return sspi_install_running() || (owned && (current.outcome == SSPI_INSTALL_RUNNING ||
        (current.outcome == SSPI_INSTALL_UNCONFIRMED && !current.ownership_released && current.submission != SSPI_SUBMIT_NONE && current.submission != SSPI_SUBMIT_REJECTED)));
}
static void clean_attempt(void) {
    if (!sspi_install_directory(directory)) return;
    const char *names[] = {"request", "request.tmp", "status", "status.tmp"};
    for (unsigned i=0; i<sizeof(names)/sizeof(*names); ++i) {
        char path[160]; snprintf(path,sizeof(path),"%s/%s",directory,names[i]); unlink(path);
    }
    rmdir(directory); /* Only these four private files; never touch a PKG. */
}
static void uncertain(int code, const char *message) {
    current.outcome = SSPI_INSTALL_UNCONFIRMED; current.api_code = code; current.status_code = code;
    snprintf(current.error,sizeof(current.error),"%s",message);
    snprintf(current.phase,sizeof(current.phase),"supervisor_error");
    /* No terminal report means the last on-disk snapshot may precede submission. */
    if (current.submission == SSPI_SUBMIT_NONE) current.submission = SSPI_SUBMIT_CALLING;
    if (!watchdog_failed) sspi_install_trace("install helper unconfirmed", message);
    watchdog_failed = true;
}
void sspi_install_loader_child(pid_t pid, void *stack) { child_stack = stack; atomic_store(&child_pid,pid); }
void sspi_install_loader_release_stack(void) { free(child_stack); child_stack = NULL; }
pid_t sspi_install_loader_waitpid(pid_t pid, int *status, int options) {
    for (;;) {
        pid_t result = waitpid(pid,status,options|WNOHANG);
        if (result > 0 || (result < 0 && errno != EINTR)) return result;
        if (sspi_install_millis() >= load_deadline) { errno=ETIMEDOUT; return -1; }
        usleep(10000);
    }
}
const struct timespec *sspi_install_loader_timeout(void) {
    static struct timespec timeout;
    uint64_t now=sspi_install_millis(), remaining=load_deadline > now ? load_deadline-now : 0;
    timeout.tv_sec=(time_t)(remaining/1000); timeout.tv_nsec=(long)(remaining%1000)*1000000;
    return &timeout;
}
void sspi_install_loader_abort(pid_t pid) {
    if (pid > 0) (void)kill(pid,SIGKILL);
    /* The supervisor retains PID and shared rfork stack until a confirmed exit. */
}
static void *spawn_helper(void *unused) {
    (void)unused;
    static char *arguments[] = {"sspi_install", directory, NULL};
    size_t length=(size_t)(sspi_install_elf_end-sspi_install_elf);
    load_deadline=sspi_install_millis()+15000;
    int result=elfldr_sanity_check(sspi_install_elf,length) ? -EINVAL :
        (int)elfldr_spawn(-1,arguments,sspi_install_elf,length);
    atomic_store(&load_result,result);
    atomic_store(&loading,false);
    return NULL;
}
static bool valid_snapshot(const SspiInstallSnapshot *s,bool final) {
    return s->magic==SSPI_INSTALL_MAGIC && s->size==sizeof(*s) &&
        memchr(s->attempt,0,sizeof(s->attempt)) && !strcmp(s->attempt,pending.attempt) &&
        (s->sequence>current.sequence || (final && s->sequence && s->sequence==current.sequence)) && s->outcome<=SSPI_INSTALL_UNCONFIRMED && s->submission<=SSPI_SUBMIT_REJECTED &&
        memchr(s->phase,0,sizeof(s->phase)) && memchr(s->content_id,0,sizeof(s->content_id)) &&
        memchr(s->status,0,sizeof(s->status)) && memchr(s->error,0,sizeof(s->error)) &&
        memchr(s->path,0,sizeof(s->path)) && !strcmp(s->path,pending.path) && s->helper_pid==sspi_install_pid() &&
        (!s->content_id[0] || sspi_install_content_id(s->content_id));
}
void sspi_install_poll(void) {
    if (!owned) return;
    uint64_t now=sspi_install_millis();
    SspiInstallSnapshot observed;
    if (!sspi_install_read(directory,"status",&observed,sizeof(observed)) && valid_snapshot(&observed,false)) {
        if (!watchdog_failed || (!strcmp(observed.phase,"finished") && observed.outcome!=SSPI_INSTALL_RUNNING)) current=observed;
        last_update=now; stop_deadline=0;
    }
    if (atomic_load(&loading)) return; /* The loader owns waitpid until its bounded launch ends. */
    if (loader_joinable) {
        pthread_join(loader_thread,NULL); loader_joinable=false;
        if (atomic_load(&load_result) <= 0) {
            if (sspi_install_pid()<=0) {
                current.outcome=SSPI_INSTALL_FAILED; current.api_code=-EIO;
                snprintf(current.error,sizeof(current.error),"The installer process could not be created"); clean_attempt();
            } else uncertain(-EIO,"The installer process could not be started");
        }
    }
    int pid=sspi_install_pid(), status=0;
    if (pid > 0) {
        pid_t result=waitpid(pid,&status,WNOHANG|WUNTRACED);
        bool exited=result==pid && (WIFEXITED(status)||WIFSIGNALED(status));
        if (result<0 && errno==ECHILD && kill(pid,0)<0 && errno==ESRCH) exited=true;
        if (exited) {
            /* An atomic final publish can race the poll's first read. Consume it
               after exit before deleting the attempt's only durable result. */
            int final=sspi_install_read(directory,"status",&observed,sizeof(observed));
            if (!final && valid_snapshot(&observed,true)) current=observed;
            if (final==-ENOENT && !current.sequence) {
                /* The helper publishes "submitting" durably before calling AppInst.
                   No status file at all means it never reached that call. */
                char reason[sizeof(current.error)];
                snprintf(reason,sizeof(reason),"%s",watchdog_failed ? current.error : "The installer exited before it started");
                current.outcome=SSPI_INSTALL_FAILED; current.submission=SSPI_SUBMIT_NONE;
                current.api_code=-ECHILD; current.status_code=0;
                snprintf(current.error,sizeof(current.error),"%.400s; no package was submitted",reason);
            } else if (current.submission==SSPI_SUBMIT_NONE && current.sequence && current.outcome==SSPI_INSTALL_RUNNING) {
                current.outcome=SSPI_INSTALL_FAILED; current.api_code=-ECHILD;
                snprintf(current.error,sizeof(current.error),"The installer exited before package submission");
            } else if (current.outcome==SSPI_INSTALL_RUNNING) uncertain(-ECHILD,"The installer exited before reporting its result");
            atomic_store(&child_pid,0); sspi_install_loader_release_stack(); clean_attempt();
            sspi_install_trace("install helper reaped",current.attempt); return;
        }
        if (result==pid && WIFSTOPPED(status)) {
            if (!stop_deadline) stop_deadline=now+5000;
            if (WSTOPSIG(status)==SIGSTOP) (void)kill(pid,SIGCONT);
        }
        if (stop_deadline && now>=stop_deadline) uncertain(-ETIMEDOUT,"The installer stopped and did not resume");
        if (now-last_update>=30000 && current.outcome==SSPI_INSTALL_RUNNING) uncertain(-ETIMEDOUT,"The installer stopped reporting progress");
        bool terminating=current.outcome!=SSPI_INSTALL_RUNNING && now-last_update>=5000;
        if ((watchdog_failed || terminating) && now>=kill_after && kill_attempts<4) {
            (void)kill(pid,SIGKILL); ++kill_attempts; kill_after=now+2000;
            sspi_install_trace("install helper kill",current.attempt);
        }
    } else if (current.outcome==SSPI_INSTALL_RUNNING) {
        uncertain(-ECHILD,"The installer did not create a process"); clean_attempt();
    }
}
int sspi_install_start(const char *path, const char *title, const char *icon, int verified_fd) {
    sspi_install_poll();
    if (sspi_install_busy()) return -EBUSY;
    if (!sspi_install_package_path(path) || strlen(title)>=sizeof(pending.title) || strlen(icon)>=sizeof(pending.icon)) return -EINVAL;
    struct stat st;
    if (fstat(verified_fd,&st) || !S_ISREG(st.st_mode)) return -EINVAL;
    owned=false; /* A failed preparation must not leave an unowned RUNNING snapshot. */
    memset(&pending,0,sizeof(pending)); memset(&current,0,sizeof(current));
    snprintf(directory,sizeof(directory),SSPI_INSTALL_ROOT "XXXXXX");
    if (!mkdtemp(directory)) return -errno;
    pending.magic=SSPI_INSTALL_MAGIC; pending.size=sizeof(pending); pending.parent=getpid();
    pending.file_size=(uint64_t)st.st_size; pending.file_inode=(uint64_t)st.st_ino;
    snprintf(pending.attempt,sizeof(pending.attempt),"%s",directory+sizeof(SSPI_INSTALL_ROOT)-1);
    snprintf(pending.path,sizeof(pending.path),"%s",path); snprintf(pending.title,sizeof(pending.title),"%s",title);
    snprintf(pending.icon,sizeof(pending.icon),"%s",icon);
    if (!sspi_install_package(verified_fd,pending.file_size,pending.content_id)) { clean_attempt(); return -EINVAL; }
    current.magic=SSPI_INSTALL_MAGIC; current.size=sizeof(current);
    snprintf(current.attempt,sizeof(current.attempt),"%s",pending.attempt);
    snprintf(current.content_id,sizeof(current.content_id),"%s",pending.content_id);
    snprintf(current.path,sizeof(current.path),"%s",pending.path);
    snprintf(current.phase,sizeof(current.phase),"launching");
    int result=sspi_install_write(directory,"request",&pending,sizeof(pending));
    if (result) { clean_attempt(); return result; }
    last_update=sspi_install_millis(); stop_deadline=kill_after=0; kill_attempts=0;
    watchdog_failed=false; owned=true; atomic_store(&child_pid,0); atomic_store(&load_result,0); atomic_store(&loading,true);
    result=pthread_create(&loader_thread,NULL,spawn_helper,NULL);
    if (result) {
        atomic_store(&loading,false); current.outcome=SSPI_INSTALL_FAILED; current.api_code=-result;
        snprintf(current.error,sizeof(current.error),"The installer loader thread could not start"); clean_attempt(); return -result;
    }
    loader_joinable=true;
    return 0;
}
int sspi_install_confirm(const char *content_id,const char *path,const char *attempt) {
    sspi_install_poll();
    if (sspi_install_running()) return -EBUSY;
    if (!owned || current.outcome!=SSPI_INSTALL_UNCONFIRMED || !*content_id || !*path || !*attempt ||
        strcmp(current.content_id,content_id) || strcmp(pending.path,path) || strcmp(pending.attempt,attempt)) return -EINVAL;
    if (!current.ownership_released) {
        current.ownership_released=1;
        sspi_install_trace("install library verified by sender",current.attempt);
    }
    return 0;
}
