#define _GNU_SOURCE
#include "proto.h"
#include "transfer.h"
#include "install.h"
#include "installed_library.h"
#include "console_tools.h"
#include "../payload/console_files.h"
#include "../payload/process_control.h"
#include "notify.h"
#include "log.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#include <sys/select.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <unistd.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sched.h>
#include <orbis/libkernel.h>

#ifndef SO_NOSIGPIPE
#define SO_NOSIGPIPE 0x0800
#endif
extern int privilege_apply(void);
static atomic_bool stopping, boot_done;
static atomic_uint clients;
static RxMutex clients_lock=RX_MUTEX_INIT, config_lock=RX_MUTEX_INIT;
static RxMutex operation_lock=RX_MUTEX_INIT;
static int client_fds[MAX_CLIENTS], listen_fd=-1, port=DEFAULT_PORT, receiver_uid=-1, boot_result, jbc_result;
static OrbisPthread client_threads[MAX_CLIENTS], worker_thread;
static bool jailbroken, writable;

static int thread_start(OrbisPthread *thread, void *(*fn)(void *), void *arg, const char *name, size_t stack) {
    OrbisPthreadAttr attr;
    int rc=scePthreadAttrInit(&attr); if (rc) return rc;
    rc=scePthreadAttrSetstacksize(&attr,stack);
    if (!rc) rc=scePthreadCreate(thread,&attr,fn,arg,name);
    int destroyed=scePthreadAttrDestroy(&attr); if (destroyed) diagnostic("thread attributes destroy rc=0x%08x",(unsigned)destroyed);
    return rc;
}
static bool probe_data(void) {
    struct stat st; int visible=stat("/user/data",&st); receiver_uid=(int)getuid();
    diagnostic("privileges uid=%d /user/data visible=%d errno=%d",receiver_uid,!visible&&S_ISDIR(st.st_mode),visible?errno:0);
    int rc=rx_mkdir(DATA_ROOT); diagnostic("data root mkdir rc=%d",rc); if (rc) return false;
    const char *probe=DATA_ROOT "/.write-test"; int fd=rx_open(probe,RX_CREATE);
    if (fd<0) { diagnostic("data root write probe open failed errno=%d",errno); return false; }
    rc=rx_write_exact(fd,"SSPI",4,0); if (rx_sync(fd)) rc=-1; if (rx_close(fd)) rc=-1; if (rx_unlink(probe)) rc=-1;
    diagnostic("data root write probe rc=%d",rc); return !rc;
}
static void *worker(void *arg) {
    (void)arg;
    writable=probe_data();
    if (receiver_uid!=0||!writable) {
        diagnostic("libjbc applying once"); jbc_result=privilege_apply();
        diagnostic("libjbc apply rc=0x%08x",(unsigned)jbc_result); writable=probe_data();
    } else diagnostic("libjbc skipped: uid=0 and data root writable");
    jailbroken=receiver_uid==0&&writable;
    boot_result=install_init();
    if (jbc_result) boot_result=jbc_result;
    if (!jailbroken && !boot_result) boot_result=-EACCES;
    install_set_privileges(jailbroken,boot_result,jbc_result);
    diagnostic("startup uid=%d jailbroken=%d writable=%d rc=0x%08x",receiver_uid,jailbroken,writable,(unsigned)boot_result);
    atomic_store_explicit(&boot_done,true,memory_order_release);
    while (!atomic_load(&stopping)) { install_worker_tick(); rx_sleep(20); }
    return NULL;
}
static int config_load(void) {
    int fd=rx_open(DATA_ROOT "/config.ini",RX_READ); if (fd<0) return DEFAULT_PORT;
    uint64_t size=0; char text[64]; int result=DEFAULT_PORT;
    if (!rx_size(fd,&size) && size<sizeof(text) && !rx_read_exact(fd,text,(size_t)size,0)) {
        int parsed=parse_port(text,(size_t)size,true); if (parsed>=0) result=parsed; else diagnostic("config.ini port invalid; using 9114");
    } else diagnostic("config.ini unreadable or oversized; using 9114");
    if (rx_close(fd)) diagnostic("config.ini close failed"); return result;
}
static int config_reply(int fd) {
    char *text=malloc(49152); if (!text) return text_reply(fd,RESP_ERROR,"out of memory");
    ModuleStatus s; install_modules(&s);
    ReceiverConfig c={port,receiver_uid,jailbroken,writable,s.bgft,s.appinst,s.userservice};
    int rc=config_json(text,49152,&c)?text_reply(fd,RESP_ERROR,"configuration JSON overflow"):text_reply(fd,RESP_DATA,text); free(text); return rc;
}
static bool empty_command(unsigned cmd) { return cmd==CMD_PING||cmd==CMD_GET_CONFIG||cmd==CMD_END_UPLOAD||cmd==CMD_INSTALL_PREFLIGHT||cmd==CMD_STOP||cmd==CMD_LIST_INSTALLED||cmd==CMD_THEME_LIST||
    cmd==CMD_KERNEL_LOG||cmd==CMD_PROCESSES||cmd==CMD_LOG_LIST; }
static uint32_t command_limit(unsigned cmd) {
    if (cmd==CMD_SHELL_REFRESH || cmd==CMD_SYSTEM_INFO) return 0;
    if (cmd==CMD_TITLE_ICON_GET) return 11;
    if (cmd==CMD_TITLE_ICON_RESTORE) return 10;
    if (cmd==CMD_TITLE_ICON_SET) return CT_MAX_PNG+10;
    if (cmd==CMD_TITLE_ICON_SET2) return 14+CT_MAX_PNG+CT_MAX_DDS;
    if (empty_command(cmd)) return 0;
    if (cmd==CMD_PROGRESS_NOTIFICATION) return 512u*1024u+16400u;
    if (cmd==CMD_INSTALL_URL) return 8193;
    if (cmd==CMD_INSTALLED_METADATA) return 10;
    if (cmd==CMD_LOG_READ) return 4+480+1;
    if (cmd==CMD_PROCESS_CONTROL) return 5+PC_NAME_MAX;
    if (cmd==CMD_START_UPLOAD) return MAX_PATH_BYTES+25;
    return MAX_PATH_BYTES+1;
}
static int dispatch(int fd, uint8_t cmd, const uint8_t *b, uint32_t n, Lane *lane) {
    const char *path;
    switch(cmd) {
        case CMD_TITLE_ICON_GET: case CMD_TITLE_ICON_SET: case CMD_TITLE_ICON_RESTORE: case CMD_TITLE_ICON_SET2:
        case CMD_SHELL_REFRESH: case CMD_SYSTEM_INFO: case CMD_KERNEL_LOG: case CMD_PROCESSES: case CMD_LOG_LIST: case CMD_LOG_READ:
        case CMD_PROCESS_CONTROL:
            return console_tools_request(fd,cmd,b,n);
        case CMD_PING: return text_reply(fd,RESP_OK,"SSPI");
        case CMD_GET_CONFIG: return config_reply(fd);
        case CMD_SET_PORT: {
            int value=parse_port((const char *)b,n,false); if (value<0) return text_reply(fd,RESP_ERROR,"port must be 1024..65535");
            char text[32]; int len=snprintf(text,sizeof(text),"port=%d\n",value);
            rx_lock(&config_lock); int rc=rx_atomic_file(DATA_ROOT "/config.ini",text,(size_t)len); rx_unlock(&config_lock);
            return text_reply(fd,rc?RESP_ERROR:RESP_OK,rc?"config write failed":"OK restart required");
        }
        case CMD_CREATE_DIR: case CMD_VERIFY_FILE:
            path=wire_string(b,n,MAX_PATH_BYTES); if (!path) return text_reply(fd,RESP_ERROR,"invalid path framing");
            return cmd==CMD_CREATE_DIR?handle_create_dir(fd,path):handle_verify(fd,path);
        case CMD_START_UPLOAD: return handle_start(fd,b,n,lane);
        case CMD_END_UPLOAD: return handle_end(fd,lane);
        case CMD_INSTALL_PREFLIGHT: return install_preflight(fd,writable);
        case CMD_INSTALL_PKG: case CMD_INSTALL_THEME: case CMD_INSTALL_URL: case CMD_CANCEL_INSTALL: case CMD_PAUSE_INSTALL: case CMD_RESUME_INSTALL:
            return install_request(fd,cmd,b,n);
        case CMD_LIST_INSTALLED: case CMD_THEME_LIST: case CMD_THEME_APPLY: case CMD_THEME_DELETE: return install_request(fd,cmd,b,n);
        case CMD_INSTALLED_METADATA: {
            const char *id=wire_string(b,n,9);
            if (!id || !valid_title_id(id) || memcmp(id,"CUSA",4)) return text_reply(fd,RESP_ERROR,"invalid installed title ID");
            uint8_t *metadata=malloc(INSTALLED_LIBRARY_MAX_METADATA);
            if (!metadata) return text_reply(fd,RESP_ERROR,"out of memory");
            size_t length=0; int rc=installed_library_metadata(id,metadata,INSTALLED_LIBRARY_MAX_METADATA,&length);
            int result=rc?text_reply(fd,RESP_ERROR,"installed title metadata unavailable"):reply(fd,RESP_DATA,metadata,(uint32_t)length);
            free(metadata); return result;
        }
        case CMD_INSTALL_STATUS:
            path=wire_string(b,n,36); if (!path||!valid_content_id(path)) return text_reply(fd,RESP_ERROR,"invalid content ID");
            return install_status_reply(fd,path);
        case CMD_TITLE_CONTEXT: { int rc=notify_context(b,n); return text_reply(fd,rc?RESP_ERROR:RESP_OK,rc?"invalid title context":"OK"); }
        case CMD_PROGRESS_NOTIFICATION: { int rc=notify_artwork(b,n); return text_reply(fd,rc?RESP_ERROR:RESP_OK,rc?"invalid notification artwork or cache write failed":"OK"); }
        case CMD_STOP: {
            if (transfer_busy() || install_busy()) return text_reply(fd,RESP_ERROR,"A transfer or installation is active. Wait for it to finish before stopping the receiver.");
            int rc=text_reply(fd,RESP_OK,"stopping"); atomic_store(&stopping,true); return rc?rc:-1;
        }
        default: return text_reply(fd,RESP_ERROR,"unknown command");
    }
}
static void *client(void *arg) {
    unsigned slot=(unsigned)(uintptr_t)arg; int fd=client_fds[slot]; Lane lane={0};
    uint8_t *buffer=malloc(UPLOAD_BUFFER);
    if (!buffer) { if (text_reply(fd,RESP_ERROR,"out of memory")) log_line("client allocation reply failed"); goto done; }
    while (!atomic_load(&stopping)) {
        ReceiveDeadline deadline; uint8_t cmd; uint32_t size; int rc=read_frame_deadline(fd,&cmd,&size,&deadline);
        if (rc) { if (rc==-2 && text_reply(fd,RESP_ERROR,"frame exceeds 8 MiB")) log_line("frame error reply failed"); break; }
        if (cmd==CMD_UPLOAD_CHUNK) { if (handle_upload_chunk_deadline(fd,size,&lane,buffer,&deadline)) break; continue; }
        if (size>command_limit(cmd)) { if (text_reply(fd,RESP_ERROR,"invalid command length")) log_line("length error reply failed"); break; }
        uint8_t *body=buffer; if (size>UPLOAD_BUFFER) { body=malloc(size); if (!body) break; }
        rc=size?recv_all_deadline(fd,body,size,&deadline):0;
        bool guarded=cmd==CMD_START_UPLOAD || cmd==CMD_INSTALL_PKG || cmd==CMD_INSTALL_THEME || cmd==CMD_INSTALL_URL || cmd==CMD_STOP || cmd==CMD_THEME_APPLY || cmd==CMD_THEME_DELETE;
        if (guarded) rx_lock(&operation_lock);
        if (atomic_load(&stopping)) rc=-1;
        if (!rc) rc=dispatch(fd,cmd,body,size,&lane);
        if (guarded) rx_unlock(&operation_lock);
        if (body!=buffer) free(body); if (rc) break;
    }
    free(buffer);
done:
    release_lane(&lane); rx_lock(&clients_lock); client_fds[slot]=-1;
    if (close(fd)) log_line("client close failed errno=%d",errno);
    rx_unlock(&clients_lock); atomic_fetch_sub(&clients,1); return NULL;
}
static int socket_options(int fd, unsigned timeout) {
    int yes=1; struct timeval tv={(time_t)timeout,0};
    if (setsockopt(fd,SOL_SOCKET,SO_NOSIGPIPE,&yes,sizeof(yes)) ||
        setsockopt(fd,SOL_SOCKET,SO_RCVTIMEO,&tv,sizeof(tv)) ||
        setsockopt(fd,SOL_SOCKET,SO_SNDTIMEO,&tv,sizeof(tv))) return -1;
    return 0;
}
static void reap_clients(bool all) {
    for (unsigned i=0;i<MAX_CLIENTS;i++) {
        rx_lock(&clients_lock); bool done=client_fds[i]<0; rx_unlock(&clients_lock);
        if (client_threads[i]&&(all||done)) {
            /* A completion flag is published before a thread's return. Join
               also waits for its epilogue before this image can be released. */
            int rc=scePthreadJoin(client_threads[i],NULL);
            if (!rc) client_threads[i]=NULL;
            else diagnostic("client thread join rc=0x%08x",(unsigned)rc);
        }
    }
}
static bool already_running(void) {
    int fd=socket(AF_INET,SOCK_STREAM,0); if (fd<0) return false;
    struct sockaddr_in a; memset(&a,0,sizeof(a)); a.sin_len=sizeof(a); a.sin_family=AF_INET; a.sin_port=htons((uint16_t)port); a.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
    bool found=false;
    if (!socket_options(fd,2) && !connect(fd,(struct sockaddr *)&a,sizeof(a)) && !reply(fd,CMD_PING,NULL,0)) {
        uint8_t cmd; uint32_t size; char text[4]; found=!read_frame(fd,&cmd,&size)&&cmd==RESP_OK&&size==4&&!recv_all(fd,text,4)&&!memcmp(text,"SSPI",4);
    }
    if (close(fd)) diagnostic("duplicate probe close errno=%d",errno); return found;
}
/* A failing accept repeats every listener tick: write it at most once a minute
   with a count of the rest, so the synced log never becomes a busy loop. */
typedef struct { bool logged; uint64_t at; unsigned repeats; } ListenerNote;
static void listener_note(ListenerNote *note, const char *event, int code) {
    uint64_t now=rx_now();
    if (note->logged && now-note->at<60000) { note->repeats++; return; }
    log_line("%s errno=%d repeats=%u clients=%u",event,code,note->repeats,atomic_load(&clients));
    note->logged=true; note->at=now; note->repeats=0;
}
int main(void) {
    int result=0;
    for (unsigned i=0;i<MAX_CLIENTS;i++) client_fds[i]=-1;
    diagnostic("BinLoader runtime initialized; starting receiver");
    int rc=thread_start(&worker_thread,worker,NULL,"sspi-installer",4u*1024u*1024u);
    if (rc) { diagnostic("installer thread creation rc=0x%08x",(unsigned)rc); notify_system("SSPI receiver: installer thread failed"); notify_tick(); return 1; }
    uint64_t start=rx_now();
    while (!atomic_load_explicit(&boot_done,memory_order_acquire)) {
        if (rx_now()-start>30000) { diagnostic("startup timed out: inspect last privilege/module diagnostic"); notify_system("SSPI receiver: privilege/module initialization timed out"); goto exit_error; }
        rx_sleep(20);
    }
    port=config_load(); listen_fd=socket(AF_INET,SOCK_STREAM,0);
    if (listen_fd<0) { diagnostic("listener socket failed errno=%d",errno); notify_system("SSPI receiver: socket failed"); goto exit_error; }
    int yes=1;
    if (setsockopt(listen_fd,SOL_SOCKET,SO_REUSEADDR,&yes,sizeof(yes))) { diagnostic("SO_REUSEADDR failed errno=%d",errno); notify_system("SSPI receiver: socket options failed"); goto exit_error; }
    struct sockaddr_in address; memset(&address,0,sizeof(address)); address.sin_len=sizeof(address); address.sin_family=AF_INET; address.sin_port=htons((uint16_t)port); address.sin_addr.s_addr=htonl(INADDR_ANY);
    if (bind(listen_fd,(struct sockaddr *)&address,sizeof(address))) {
        diagnostic("bind port=%d failed errno=%d",port,errno);
        char text[96]; if (already_running()) snprintf(text,sizeof(text),"SSPI receiver is already running"); else snprintf(text,sizeof(text),"Port %d is in use",port);
        notify_system(text); goto exit_error;
    }
    if (listen(listen_fd,48)) { diagnostic("listen failed errno=%d",errno); notify_system("SSPI receiver: listen failed"); goto exit_error; }
    char ready[160];
    if (boot_result) snprintf(ready,sizeof(ready),"SSPI receiver: %s failed (0x%08x); inspect GET_CONFIG",!jailbroken?"privilege/data root":"installer initialization",(unsigned)boot_result);
    else snprintf(ready,sizeof(ready),"SSPI receiver ready \xc2\xb7 port %d",port);
    notify_system(ready); diagnostic("listener ready port=%d version=" VERSION,port);
    ListenerNote accept_note={0};
    while (!atomic_load(&stopping)) {
        reap_clients(false);
        notify_tick(); fd_set readable; FD_ZERO(&readable); FD_SET(listen_fd,&readable); struct timeval wait={0,100000};
        rc=select(listen_fd+1,&readable,NULL,NULL,&wait); if (rc<0) { if (errno==EINTR) continue; diagnostic("listener select failed errno=%d",errno); break; } if (!rc) continue;
        int fd=accept(listen_fd,NULL,NULL);
        if (fd<0) { int code=errno; if (code!=EINTR) { listener_note(&accept_note,"accept failed",code); rx_sleep(100); } continue; }
        if (socket_options(fd,30)) { log_line("client socket options failed errno=%d",errno); if (close(fd)) log_line("client close failed"); continue; }
        rx_lock(&clients_lock); unsigned slot=0; while (slot<MAX_CLIENTS&&(client_fds[slot]>=0||client_threads[slot])) slot++;
        if (slot<MAX_CLIENTS) { client_fds[slot]=fd; atomic_fetch_add(&clients,1); } rx_unlock(&clients_lock);
        if (slot==MAX_CLIENTS) { if (text_reply(fd,RESP_ERROR,"receiver client capacity busy")) log_line("busy reply failed"); if (close(fd)) log_line("busy close failed"); continue; }
        rc=thread_start(&client_threads[slot],client,(void *)(uintptr_t)slot,"sspi-client",1024u*1024u);
        if (rc) { diagnostic("client thread rc=0x%08x",(unsigned)rc); rx_lock(&clients_lock); client_fds[slot]=-1; rx_unlock(&clients_lock); atomic_fetch_sub(&clients,1); if (close(fd)) log_line("thread failure close failed"); }
    }
    goto stop_threads;
exit_error:
    result=1;
stop_threads:
    atomic_store(&stopping,true); notify_tick();
    if (listen_fd>=0 && close(listen_fd)) log_line("listener close failed errno=%d",errno); listen_fd=-1;
    rx_lock(&clients_lock);
    for (unsigned i=0;i<MAX_CLIENTS;i++) if (client_fds[i]>=0 && shutdown(client_fds[i],SHUT_RDWR) && errno!=ENOTCONN) log_line("client shutdown failed errno=%d",errno);
    rx_unlock(&clients_lock);
    /* The loader may release this image when its entry returns. Drain every
       thread first; _exit would terminate the process hosting GoldHEN. */
    reap_clients(true);
    while ((rc=scePthreadJoin(worker_thread,NULL))!=0) {
        diagnostic("installer thread join rc=0x%08x",(unsigned)rc); rx_sleep(1000);
    }
    for (unsigned i=0;i<MAX_CLIENTS;i++) while (client_threads[i]) { reap_clients(true); rx_sleep(1000); }
    diagnostic("receiver threads stopped"); return result;
}
