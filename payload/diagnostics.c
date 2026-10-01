/* Shared, read-only diagnostics. The PS4 receiver is freestanding, so this file
   only uses the libc calls both receivers import (snprintf, mem*, str*, malloc). */
#include "diagnostics.h"
#include "console_files.h"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void dx_json_string(char *out, size_t cap, const char *text, size_t max) {
    size_t n=0; if (cap<3) { if (cap) out[0]=0; return; }
    out[n++]='"';
    for (size_t i=0;i<max && text[i] && n+4<cap;i++) {
        unsigned char c=(unsigned char)text[i];
        if (c=='"' || c=='\\') { out[n++]='\\'; out[n++]=(char)c; }
        else out[n++]=c<32 || c>126 ? '?' : (char)c;
    }
    out[n++]='"'; out[n]=0;
}

/* ------------------------------------------------------------------ kernel log */

/* /dev/klog reads are destructive, so everything drained is kept here and every
   request returns the whole history, like a msgbuf snapshot. */
static char *klog_ring;
static size_t klog_used;
static bool klog_dropped;

void dx_klog_reset(void) {
    dx_platform_lock();
    free(klog_ring); klog_ring=NULL; klog_used=0; klog_dropped=false;
    dx_platform_unlock();
}
static void klog_append(const char *text, size_t n) {
    if (!n) return;
    if (!klog_ring && !(klog_ring=malloc(DX_TEXT_MAX))) return;
    if (n>=DX_TEXT_MAX) { text+=n-DX_TEXT_MAX; n=DX_TEXT_MAX; klog_used=0; klog_dropped=true; }
    if (klog_used+n>DX_TEXT_MAX) {
        size_t drop=klog_used+n-DX_TEXT_MAX;
        memmove(klog_ring,klog_ring+drop,klog_used-drop); klog_used-=drop; klog_dropped=true;
    }
    memcpy(klog_ring+klog_used,text,n); klog_used+=n;
}
/* NUL bytes appear in wrapped message buffers; they would end the text early. */
static void printable(char *text, size_t n) { for (size_t i=0;i<n;i++) if (!text[i]) text[i]='\n'; }

static int prepend_header(char *out, size_t cap, size_t text, const char *header, size_t *size) {
    size_t h=strlen(header);
    if (h+1+text>cap) return -1;
    memmove(out+h+1,out,text); memcpy(out,header,h); out[h]='\n';
    *size=h+1+text; return 0;
}

/* No strerror import is required by the freestanding receiver. Keep the native
   number even for errors this small, shared table does not know. */
static const char *error_name(int error) {
    switch (error) {
        case EPERM: return "Operation not permitted";
        case EACCES: return "Permission denied";
        case ENOENT: return "No such file or directory";
        case ENOSYS: return "Function not implemented";
        case ENOTSUP: return "Operation not supported";
        case EBUSY: return "Device busy";
        case ENOMEM: return "Cannot allocate memory";
        case EIO: return "Input/output error";
        case ENXIO: return "Device not configured";
        case ENODEV: return "No such device";
        case EINVAL: return "Invalid argument";
        case EOVERFLOW: return "Value too large";
        case EBADF: return "Bad file descriptor";
        case EMFILE: return "Too many open files";
        case ENFILE: return "Too many open files in system";
        default: return "Unknown error";
    }
}

int dx_kernel_log(char *out, size_t cap, size_t *size, char *error, size_t error_cap) {
    enum { reserve=512 };
    if (cap<reserve+4096) { snprintf(error,error_cap,"Kernel log buffer is too small."); return -1; }
    size_t room=cap-reserve; if (room>DX_TEXT_MAX) room=DX_TEXT_MAX;
    size_t n=0; char header[reserve]; int msgbuf_error=0, klog_error=0;
    dx_platform_lock();
    /* kern.msgbuf is a snapshot: it never competes with GoldHEN, etaHEN or klogsrv for /dev/klog. */
    if (!dx_platform_msgbuf(out,room,&n,&msgbuf_error)) {
        dx_platform_unlock();
        if (n>room) n=room;
        printable(out,n);
        snprintf(header,sizeof(header),"{\"source\":\"msgbuf\",\"bytes\":%lu,\"busy\":false,\"dropped\":false}",(unsigned long)n);
        if (prepend_header(out,cap,n,header,size)) { snprintf(error,error_cap,"Kernel log reply overflow."); return -1; }
        return 0;
    }
    char *chunk=malloc(64u*1024u);
    if (!chunk) { dx_platform_unlock(); snprintf(error,error_cap,"Not enough memory to read the kernel log."); return -1; }
    int state=0; uint64_t until=dx_platform_ms()+250;
    for (;;) {
        size_t got=0; state=dx_platform_klog_drain(chunk,64u*1024u,&got,&klog_error);
        printable(chunk,got); klog_append(chunk,got);
        if (state) break;
        if (!got || dx_platform_ms()>=until) break;
    }
    if (state==-1 && !klog_used) {
        dx_platform_unlock(); free(chunk);
        snprintf(error,error_cap,"The receiver couldn't read the kernel log: kern.msgbuf: %s (%d); /dev/klog: %s (%d).",
            error_name(msgbuf_error),msgbuf_error,error_name(klog_error),klog_error);
        return -1;
    }
    n=klog_used<room ? klog_used : room;
    if (n) memcpy(out,klog_ring+klog_used-n,n);
    snprintf(header,sizeof(header),"{\"source\":\"klog\",\"bytes\":%lu,\"busy\":%s,\"dropped\":%s}",
        (unsigned long)n,state==-2?"true":"false",klog_dropped?"true":"false");
    dx_platform_unlock(); free(chunk);
    if (prepend_header(out,cap,n,header,size)) { snprintf(error,error_cap,"Kernel log reply overflow."); return -1; }
    return 0;
}

/* ------------------------------------------------------------------ log files */

static bool ends_with(const char *name, const char *suffix) {
    size_t n=strlen(name), s=strlen(suffix);
    if (s>n) return false;
    for (size_t i=0;i<s;i++) {
        char c=name[n-s+i]; if (c>='A' && c<='Z') c=(char)(c+32);
        if (c!=suffix[i]) return false;
    }
    return true;
}
static bool contains_folded(const char *name, const char *word) {
    size_t w=strlen(word);
    for (const char *p=name;*p;p++) {
        size_t i=0;
        while (i<w && p[i]) { char c=p[i]; if (c>='A' && c<='Z') c=(char)(c+32); if (c!=word[i]) break; i++; }
        if (i==w) return true;
    }
    return false;
}
int dx_file_kind(const char *name) {
    static const char *const crash[]={".orbisdmp",".orbisstate",".prosperodmp",".prosperostate",".core",".dmp",".nxdp"};
    static const char *const logs[]={".log",".txt",".out"};
    static const char *const config[]={".ini",".cfg",".conf",".json",".lst",".xml"};
    for (size_t i=0;i<sizeof(crash)/sizeof(crash[0]);i++) if (ends_with(name,crash[i])) return 3;
    for (size_t i=0;i<sizeof(logs)/sizeof(logs[0]);i++) if (ends_with(name,logs[i])) return 1;
#if defined(SSPI_BINLOADER) || defined(CT_PS4)
    /* receiver.log.1 and the numbered rotations used by other PS4 payloads. */
    const char *rotation=strrchr(name,'.');
    if (rotation && rotation[1] && (size_t)(rotation-name)<480) {
        const char *p=rotation+1; while (*p>='0' && *p<='9') p++;
        if (!*p) {
            char base[480]; size_t n=(size_t)(rotation-name); memcpy(base,name,n); base[n]=0;
            for (size_t i=0;i<sizeof(logs)/sizeof(logs[0]);i++) if (ends_with(base,logs[i])) return 1;
        }
    }
#endif
    for (size_t i=0;i<sizeof(config)/sizeof(config[0]);i++) if (ends_with(name,config[i])) return 2;
    if (contains_folded(name,"coredump") || contains_folded(name,"systemcrash")) return 3;
    return 0;
}
static const char *const kind_names[]={"","log","config","crash"};

/* Game content, upload staging and caches hold thousands of unrelated files. */
static bool skipped_directory(const char *path, const char *name) {
#if defined(SSPI_BINLOADER) || defined(CT_PS4)
    /* These bounded roots were already scanned before the broad data folders. */
    if (!strcmp(path,"/user/data/sspi-receiver") || !strcmp(path,"/data/GoldHEN")) return true;
#endif
    static const char *const names[]={"homebrew","games","backports","upload","artwork","tmp","cache","sce_sys","app0","trophy"};
    for (size_t i=0;i<sizeof(names)/sizeof(names[0]);i++) {
        const char *a=name,*b=names[i];
        while (*a && *b) { char c=*a; if (c>='A' && c<='Z') c=(char)(c+32); if (c!=*b) break; a++; b++; }
        if (!*a && !*b) return true;
    }
    (void)path; return false;
}

typedef struct {
    char *out; size_t cap, at;
    unsigned files, entries, depth;
    bool truncated, failed;
    uint64_t until;
    char dir[480];
} DxScan;

static void scan_add(DxScan *s, const char *path, uint64_t size, int64_t modified, int kind) {
    if (s->files==DX_LOG_FILES) { s->truncated=true; return; }
    char quoted[1024]; ct_json_quote(quoted,sizeof(quoted),path);
    int n=snprintf(s->out+s->at,s->cap-s->at,"%s{\"path\":%s,\"size\":%llu,\"modified\":%lld,\"kind\":\"%s\"}",
        s->files?",":"",quoted,(unsigned long long)size,(long long)modified,kind_names[kind]);
    if (n<0 || (size_t)n>=s->cap-s->at) { s->truncated=true; return; }
    s->at+=(size_t)n; s->files++;
}
static bool plain_name(const char *name) {
    if (!name[0] || !strcmp(name,".") || !strcmp(name,"..")) return false;
    for (const char *p=name;*p;p++) if ((unsigned char)*p<32 || (unsigned char)*p>126 || *p=='/' || *p=='\\' || *p=='"') return false;
    return true;
}
static int scan_visit(const char *name, void *context);
static void scan_directory(DxScan *s, unsigned depth) {
    if (dx_platform_ms()>=s->until) { s->truncated=true; return; }
    unsigned saved=s->depth; s->depth=depth;
    if (dx_platform_list(s->dir,scan_visit,s)) s->failed=true;
    s->depth=saved;
}
static int scan_visit(const char *name, void *context) {
    DxScan *s=context;
    if (++s->entries>8192 || dx_platform_ms()>=s->until) { s->truncated=true; return 1; }
    if (!plain_name(name)) return 0;
    size_t base=strlen(s->dir);
    if (base+1+strlen(name)>=sizeof(s->dir)) return 0;
    s->dir[base]='/'; memcpy(s->dir+base+1,name,strlen(name)+1);
    uint64_t size=0; int64_t modified=0;
    int kind=dx_platform_lstat(s->dir,&size,&modified);
    if (kind==1) { int file=dx_file_kind(name); if (file) scan_add(s,s->dir,size,modified,file); }
    else if (kind==2 && s->depth>1 && !skipped_directory(s->dir,name)) scan_directory(s,s->depth-1);
    s->dir[base]=0;
    return s->files==DX_LOG_FILES || s->truncated ? 1 : 0;
}

int dx_log_list(char *out, size_t cap, size_t *size, char *error, size_t error_cap) {
    static const struct { const char *path; unsigned depth; } roots[]={
#if defined(SSPI_BINLOADER) || defined(CT_PS4)
        {"/user/data/sspi-receiver",1},{"/data/GoldHEN",3},
#endif
        {"/data",3},{"/user/data",2}};
    if (cap<1024) { snprintf(error,error_cap,"Log list buffer is too small."); return -1; }
    DxScan s; memset(&s,0,sizeof(s));
    s.out=out; s.cap=cap-256; s.until=dx_platform_ms()+2000;
    uint64_t started=dx_platform_ms();
    int n=snprintf(out,s.cap,"{\"files\":["); s.at=(size_t)n;
    unsigned scanned=0;
    for (size_t i=0;i<sizeof(roots)/sizeof(roots[0]);i++) {
#if defined(SSPI_BINLOADER) || defined(CT_PS4)
        if (s.truncated) break;
        if (ct_kind(roots[i].path)!=2) continue;
#else
        if (dx_platform_lstat(roots[i].path,NULL,NULL)!=2) continue;
#endif
        snprintf(s.dir,sizeof(s.dir),"%s",roots[i].path);
        scan_directory(&s,roots[i].depth); scanned++;
    }
    n=snprintf(out+s.at,cap-s.at,"],\"truncated\":%s,\"incomplete\":%s,\"roots\":%u,\"elapsedMs\":%llu}",
        s.truncated?"true":"false",s.failed?"true":"false",scanned,(unsigned long long)(dx_platform_ms()-started));
    if (n<0 || (size_t)n>=cap-s.at) { snprintf(error,error_cap,"Log list reply overflow."); return -1; }
    *size=s.at+(size_t)n; return 0;
}

/* Only text files under payload data folders; ct_kind rejects traversal and linked ancestors. */
bool dx_readable_path(const char *path) {
    if (!path || strlen(path)>=480) return false;
    if (strncmp(path,"/data/",6) && strncmp(path,"/user/data/",11)) return false;
    const char *name=strrchr(path,'/');
    int kind=dx_file_kind(name ? name+1 : path);
    return (kind==1 || kind==2) && ct_kind(path)==1;
}

int dx_log_read(const uint8_t *body, size_t n, char *out, size_t cap, size_t *size, char *error, size_t error_cap) {
    if (!body || n<6 || body[n-1] || memchr(body+4,0,n-5)) { snprintf(error,error_cap,"Invalid log read request."); return -1; }
    uint32_t limit=(uint32_t)body[0]|((uint32_t)body[1]<<8)|((uint32_t)body[2]<<16)|((uint32_t)body[3]<<24);
    const char *path=(const char *)body+4;
    if (!limit || limit>DX_TEXT_MAX) limit=DX_TEXT_MAX;
    if (!dx_readable_path(path)) { snprintf(error,error_cap,"Only log and settings files in payload data folders can be read."); return -1; }
    uint64_t total=0; int64_t modified=0;
    if (dx_platform_lstat(path,&total,&modified)!=1) { snprintf(error,error_cap,"The log file is no longer available."); return -1; }
    size_t take=total<limit ? (size_t)total : limit;
    enum { reserve=1536 };
    if (cap<reserve || take>cap-reserve) { snprintf(error,error_cap,"Log reply buffer is too small."); return -1; }
    uint64_t offset=total-take;
    if (take && ct_read_at(path,(uint8_t *)out,take,offset)) { snprintf(error,error_cap,"The log file could not be read."); return -1; }
    printable(out,take);
    char quoted[1024],header[reserve]; ct_json_quote(quoted,sizeof(quoted),path);
    snprintf(header,sizeof(header),"{\"path\":%s,\"size\":%llu,\"offset\":%llu,\"bytes\":%lu,\"modified\":%lld}",
        quoted,(unsigned long long)total,(unsigned long long)offset,(unsigned long)take,(long long)modified);
    if (prepend_header(out,cap,take,header,size)) { snprintf(error,error_cap,"Log reply overflow."); return -1; }
    return 0;
}
