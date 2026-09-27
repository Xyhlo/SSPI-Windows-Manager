#define _GNU_SOURCE
#include "platform.h"
#include "notify.h"
#include "pkg.h"
#include "runtime.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>
#include <sched.h>
#include <poll.h>
#include <time.h>
#include <orbis/libkernel.h>

void rx_sleep(unsigned ms) { int rc=sceKernelUsleep(ms*1000u); if (rc) payload_debug("SSPI sleep rc=%d\n",rc); }
uint64_t rx_now(void) { return sceKernelGetProcessTime()/1000; }
uint64_t rx_wall_time(void) { time_t now=time(NULL); return now<0?0:(uint64_t)now; }
static int parents(const char *path, bool create) {
    char copy[2200]; size_t len=strlen(path); if (!len||len>=sizeof(copy)||path[0]!='/') return -1;
    memcpy(copy,path,len+1);
    for (char *p=copy+1;*p;p++) if (*p=='/') {
        *p=0; struct stat st;
        if (lstat(copy,&st)) { if (!create||errno!=ENOENT||(mkdir(copy,0775)&&errno!=EEXIST)||lstat(copy,&st)) return -1; }
        if (!S_ISDIR(st.st_mode)||S_ISLNK(st.st_mode)) return -1; *p='/';
    }
    return 0;
}
int rx_open(const char *p, int mode) {
    if (parents(p,false)) return -1;
    int flags=mode==RX_READ?O_RDONLY:O_RDWR|O_CREAT|(mode==RX_CREATE?O_TRUNC:0);
    int fd=open(p,flags|O_NOFOLLOW,0664); if (fd<0) return -1;
    struct stat st; if (fstat(fd,&st)||!S_ISREG(st.st_mode)) { if (close(fd)) payload_debug("SSPI close failed\n"); return -1; } return fd;
}
int rx_close(int fd) { return close(fd); }
int rx_sync(int fd) { return fsync(fd); }
int rx_resize(int fd, uint64_t n) { return n>INT64_MAX?-1:ftruncate(fd,(off_t)n); }
int rx_size(int fd, uint64_t *n) { struct stat st; if (fstat(fd,&st)||!S_ISREG(st.st_mode)||st.st_size<0) return -1; *n=(uint64_t)st.st_size; return 0; }
int rx_stat(const char *p, uint64_t *n, uint64_t *stamp) { struct stat st; if (parents(p,false)||lstat(p,&st)||!S_ISREG(st.st_mode)||st.st_size<0) return -1; *n=(uint64_t)st.st_size; *stamp=(uint64_t)st.st_mtim.tv_sec*1000000000u+(uint64_t)st.st_mtim.tv_nsec; return 0; }
int rx_mkdir(const char *p) {
    if (parents(p,true)) return -1; struct stat st;
    if (lstat(p,&st)) { if (errno!=ENOENT||(mkdir(p,0775)&&errno!=EEXIST)||lstat(p,&st)) return -1; }
    return S_ISDIR(st.st_mode)&&!S_ISLNK(st.st_mode)?0:-1;
}
int rx_unlink(const char *p) { return parents(p,false)?-1:unlink(p); }
int rx_rename(const char *from, const char *to) { return parents(from,false)||parents(to,false)?-1:rename(from,to); }
int64_t rx_pread(int fd, void *p, size_t n, uint64_t off) { ssize_t rc; do { rc=pread(fd,p,n,(off_t)off); } while (rc<0&&errno==EINTR); return rc; }
int64_t rx_pwrite(int fd, const void *p, size_t n, uint64_t off) { ssize_t rc; do { rc=pwrite(fd,p,n,(off_t)off); } while (rc<0&&errno==EINTR); return rc; }
int64_t rx_send(int fd, const void *p, size_t n) { ssize_t rc; do { rc=send(fd,p,n,0); } while (rc<0&&errno==EINTR); return rc; }
int64_t rx_recv(int fd, void *p, size_t n, uint64_t deadline) {
    for (;;) {
        uint64_t now=rx_now(); if (now>=deadline) return -1;
        struct pollfd wait={fd,POLLIN,0}; int rc=poll(&wait,1,(int)(deadline-now));
        if (rc<0&&errno==EINTR) continue; if (rc<=0) return -1;
        ssize_t got=recv(fd,p,n,MSG_DONTWAIT);
        if (got<0&&(errno==EINTR||errno==EAGAIN||errno==EWOULDBLOCK)) continue;
        return got;
    }
}
int platform_toast(const char *tid, const char *text) {
    OrbisNotificationRequest n; memset(&n,0,sizeof(n)); n.targetId=-1; n.useIconImageUri=1;
    _Static_assert(sizeof(OrbisNotificationRequest)==0xc30,"notification ABI");
    snprintf(n.message,sizeof(n.message),"%s",text);
    char path[128]; uint64_t size, stamp; snprintf(path,sizeof(path),ART_ROOT "/%s.png",tid);
    if (valid_title_id(tid)&&!rx_stat(path,&size,&stamp)) snprintf(n.iconUri,sizeof(n.iconUri),"file://%s",path);
    else snprintf(n.iconUri,sizeof(n.iconUri),"cxml://psnotification/tex_default_icon_download");
    return sceKernelSendNotificationRequest(0,&n,sizeof(n),0);
}
