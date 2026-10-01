/* PS4 system information, processes and the platform hooks behind the shared
   diagnostics. Functions not every firmware or process exports are resolved on
   first use; when one is missing its value is reported as null. */
#include "system_ps4.h"
#include "installed_library.h"
#include "native_stat.h"
#include "platform.h"
#include "runtime.h"
#include "../payload/console_files.h"
#include "../payload/diagnostics.h"
#include "../payload/process_control.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

extern int sceKernelGetdents(int fd, void *buffer, size_t size);

typedef int (*SysctlFn)(const int *, unsigned, void *, size_t *, const void *, size_t);
typedef int (*SysctlByNameFn)(const char *, void *, size_t *, const void *, size_t);
typedef int (*TemperatureFn)(int *);
typedef int (*SensorFn)(int, int *);
typedef long (*FrequencyFn)(void);
typedef int (*AppInfoFn)(int, void *);
typedef int (*KillFn)(int, int);
typedef int (*GetpidFn)(void);
typedef int (*KillAppFn)(uint32_t, int32_t, int32_t, int32_t);
typedef int (*FsStatFn)(void *, long, int);
typedef int (*HostNameFn)(char *, size_t);
typedef struct RawIfaddrs { struct RawIfaddrs *next; char *name; unsigned flags; uint8_t *address, *netmask, *destination; void *data; } RawIfaddrs;
typedef int (*IfaddrsFn)(RawIfaddrs **);
typedef void (*FreeIfaddrsFn)(RawIfaddrs *);

static void *symbol(const char *name) { return rx_optional_symbol(name); }
static bool sysctl_name(const char *name, void *value, size_t *size) {
    SysctlByNameFn fn=(SysctlByNameFn)symbol("sysctlbyname");
    return fn && !fn(name,value,size,NULL,0);
}
static bool sysctl_u64(const char *name, uint64_t *value) {
    uint64_t wide=0; size_t size=sizeof(wide);
    if (!sysctl_name(name,&wide,&size)) return false;
    if (size==4) { uint32_t narrow; memcpy(&narrow,&wide,4); *value=narrow; return true; }
    if (size==8) { *value=wide; return true; }
    return false;
}

/* ------------------------------------------------------------------ diagnostics hooks */

static RxMutex diagnostics_lock=RX_MUTEX_INIT;
void dx_platform_lock(void) { rx_lock(&diagnostics_lock); }
void dx_platform_unlock(void) { rx_unlock(&diagnostics_lock); }
uint64_t dx_platform_ms(void) { return rx_now(); }

typedef struct { DxVisit visit; void *context; } ListContext;
int dx_platform_list(const char *dir, DxVisit visit, void *context) {
    int fd=open(dir,O_RDONLY|O_DIRECTORY); if (fd<0) return -1;
    uint8_t buffer[8192]; int result=0;
    for (;;) {
        int n=sceKernelGetdents(fd,buffer,sizeof(buffer));
        if (n<0 || (size_t)n>sizeof(buffer)) { result=-1; break; }
        if (!n) break;
        int parsed=installed_library_parse_dirents(buffer,(size_t)n,visit,context);
        if (parsed<0) { result=-1; break; }
        if (parsed>0) break;
    }
    if (close(fd)) result=-1;
    return result;
}
int dx_platform_lstat(const char *path, uint64_t *size, int64_t *modified) {
    NativeStat st;
    if (rx_native_lstat(path,&st)) return errno==ENOENT || errno==ENOTDIR ? 0 : -1;
    unsigned type=st.mode&0170000u;
    if (type==0040000u) return 2;
    if (type!=0100000u || st.size<0) return -1;
    if (size) *size=(uint64_t)st.size;
    if (modified) *modified=st.modified.seconds;
    return 1;
}
typedef struct {
    SysctlByNameFn byname;
    SysctlFn raw;
    const int *mib; unsigned count;
    void *out; size_t *size;
    const void *input; size_t input_size;
} LogQuery;

static int log_sysctl(void *context) {
    LogQuery *q=context;
    return q->byname ? q->byname("kern.msgbuf",q->out,q->size,NULL,0)
        : q->raw(q->mib,q->count,q->out,q->size,q->input,q->input_size);
}
static int log_query(LogQuery *q, int *error) {
    size_t capacity=*q->size;
    errno=0; int rc=log_sysctl(q);
    /* sysctlbyname may perform multiple syscalls. Only the raw sysctl path is
       elevated, with resolution and memory allocation outside that window. */
    if (rc && !q->byname && (errno==EPERM || errno==EACCES)) {
        *q->size=capacity;
        if (privilege_call(log_sysctl,q,&rc)) rc=-1;
    }
    *error=rc ? (errno ? errno : EIO) : 0;
    return rc ? -1 : 0;
}
static int msgbuf_snapshot(LogQuery *q, char *out, size_t cap, size_t *size, int *error) {
    size_t length=0; q->out=NULL; q->size=&length;
    if (log_query(q,error)) return -1;
    if (length>64u*1024u*1024u) { *error=EOVERFLOW; return -1; }
    /* A readable but empty snapshot does not justify a destructive klog read. */
    if (!length) { *size=0; return 0; }
    size_t allocated=length+4096; length=allocated;
    char *buffer=malloc(allocated); if (!buffer) { *error=ENOMEM; return -1; }
    q->out=buffer;
    if (log_query(q,error)) { free(buffer); return -1; }
    if (length>allocated) { free(buffer); *error=EOVERFLOW; return -1; }
    while (length && !buffer[length-1]) length--;
    size_t take=length<cap ? length : cap;
    memcpy(out,buffer+length-take,take); free(buffer);
    *size=take; return 0;
}
int dx_platform_msgbuf(char *out, size_t cap, size_t *size, int *error) {
    *size=0; *error=ENOSYS;
    LogQuery q={0};
    q.byname=(SysctlByNameFn)symbol("sysctlbyname");
    if (q.byname && !msgbuf_snapshot(&q,out,cap,size,error)) return 0;
    q.byname=NULL; q.raw=(SysctlFn)symbol("sysctl");
    if (!q.raw) return -1;
    /* CTL_SYSCTL, CTL_SYSCTL_NAME2OID: the OID for kern.msgbuf is not stable. */
    const int name2oid[2]={0,3}; int mib[24]; size_t length=sizeof(mib);
    q.mib=name2oid; q.count=2; q.out=mib; q.size=&length;
    q.input="kern.msgbuf"; q.input_size=sizeof("kern.msgbuf")-1;
    if (log_query(&q,error)) return -1;
    if (!length || length>sizeof(mib) || length%sizeof(mib[0])) { *error=EINVAL; return -1; }
    q.mib=mib; q.count=(unsigned)(length/sizeof(mib[0])); q.input=NULL; q.input_size=0;
    return msgbuf_snapshot(&q,out,cap,size,error);
}
static int log_open(void *context) {
    (void)context;
    return open("/dev/klog",O_RDONLY|O_NONBLOCK);
}
int dx_platform_klog_drain(char *out, size_t cap, size_t *size, int *error) {
    *size=0; *error=0;
    errno=0; int fd=log_open(NULL);
    if (fd<0 && (errno==EPERM || errno==EACCES)) {
        if (privilege_call(log_open,NULL,&fd)) {
            *error=errno ? errno : EIO;
            if (fd>=0) close(fd);
            return -1;
        }
    }
    if (fd<0) { *error=errno ? errno : EIO; return *error==EBUSY ? -2 : -1; }
    size_t n=0;
    while (n<cap) {
        ssize_t got=read(fd,out+n,cap-n);
        if (got>0) { n+=(size_t)got; continue; }
        if (got<0 && errno==EINTR) continue;
        if (got<0 && errno!=EAGAIN && errno!=EWOULDBLOCK) *error=errno ? errno : EIO;
        break;
    }
    if (close(fd) && !*error) *error=errno ? errno : EIO;
    *size=n; return *error ? -1 : 0;
}

/* ------------------------------------------------------------------ processes */

/* The PS4 kernel returns FreeBSD's amd64 kinfo_proc; these offsets are checked
   against ki_structsize before any field is read. */
enum {
    KINFO_SIZE=1088, KI_PID=72, KI_PPID=76, KI_UID=168, KI_SIZE=256, KI_RSSIZE=264,
    KI_RUNTIME=328, KI_START=336, KI_STAT=388, KI_COMM=447, KI_COMM_LENGTH=20, KI_THREADS=596
};
static int32_t i32(const uint8_t *p) { int32_t v; memcpy(&v,p,4); return v; }
static uint64_t u64(const uint8_t *p) { uint64_t v; memcpy(&v,p,8); return v; }
static const char *state_name(uint8_t state) {
    static const char *const names[]={"unknown","starting","running","sleeping","stopped","zombie","waiting","locked"};
    return state<8 ? names[state] : names[0];
}
static uint8_t *process_table(size_t *size) {
    SysctlFn fn=(SysctlFn)symbol("sysctl"); if (!fn) return NULL;
    const int mib[4]={1,14,8,0}; /* CTL_KERN, KERN_PROC, KERN_PROC_PROC */
    size_t length=0;
    if (fn(mib,4,NULL,&length,NULL,0) || !length) return NULL;
    for (unsigned attempt=0;attempt<3;attempt++) {
        length+=length/4+KINFO_SIZE*16;
        uint8_t *table=malloc(length); if (!table) return NULL;
        size_t got=length;
        if (!fn(mib,4,table,&got,NULL,0)) { *size=got; return table; }
        free(table);
        if (errno!=ENOMEM) return NULL;
    }
    return NULL;
}
static void app_info(int pid, char title[10], uint32_t *app_id) {
    title[0]=0; if (app_id) *app_id=0;
    AppInfoFn fn=(AppInfoFn)symbol("sceKernelGetAppInfo"); if (!fn) return;
    uint8_t info[128]; memset(info,0,sizeof(info));
    if (fn(pid,info)) return;
    memcpy(title,info+16,9); title[9]=0; /* OrbisAppInfo.TitleId */
    if (!ct_valid_id(title)) title[0]=0;
    if (app_id) memcpy(app_id,info,4); /* OrbisAppInfo.AppId */
}
static void app_title(int pid, char title[10]) { app_info(pid,title,NULL); }
static bool record_valid(const uint8_t *table, size_t length, size_t offset) {
    return offset+KINFO_SIZE<=length && i32(table+offset)==KINFO_SIZE;
}

/* The process table as the stop rules see it. The PS4 list has no auth IDs, so only
   apps qualify there (payloads are recognised by their elfldr.elf parent on the PS5). */
static PcProcess *control_table(const uint8_t *table, size_t length, size_t *count) {
    size_t capacity=length/KINFO_SIZE+1, n=0;
    PcProcess *items=calloc(capacity,sizeof(PcProcess)); if (!items) return NULL;
    for (size_t offset=0;record_valid(table,length,offset) && n<capacity;offset+=KINFO_SIZE) {
        const uint8_t *p=table+offset; PcProcess *item=&items[n++];
        item->pid=i32(p+KI_PID); item->ppid=i32(p+KI_PPID);
        size_t name_length=0; while (name_length<KI_COMM_LENGTH && name_length<PC_NAME_MAX-1 && p[KI_COMM+name_length]) name_length++;
        memcpy(item->name,p+KI_COMM,name_length); item->name[name_length]=0;
        app_info(item->pid,item->title,&item->app_id);
    }
    *count=n; return items;
}
static int own_pid(void) { GetpidFn fn=(GetpidFn)symbol("getpid"); return fn ? fn() : -1; }

int ps4_processes_json(char *out, size_t cap) {
    size_t length=0; uint8_t *table=process_table(&length);
    if (!table) return -1;
    if (!record_valid(table,length,0)) { free(table); return -2; }
    size_t controls=0; PcProcess *control=control_table(table,length,&controls);
    if (!control) { free(table); return -1; }
    int self=own_pid();
    uint64_t page=16384; sysctl_u64("hw.pagesize",&page);
    size_t at=0; unsigned count=0; bool truncated=false;
    int n=snprintf(out,cap,"{\"processes\":["); if (n<0 || (size_t)n>=cap) { free(table); return -1; }
    at=(size_t)n;
    for (size_t offset=0;record_valid(table,length,offset);offset+=KINFO_SIZE) {
        const uint8_t *p=table+offset;
        char name[64],title[10],quoted_title[16]="null",kind[16]="null";
        dx_json_string(name,sizeof(name),(const char *)p+KI_COMM,KI_COMM_LENGTH);
        const char *can=pc_kind_name(pc_classify(control,controls,self,i32(p+KI_PID),NULL,0));
        if (can) snprintf(kind,sizeof(kind),"\"%s\"",can);
        app_title(i32(p+KI_PID),title);
        if (title[0]) snprintf(quoted_title,sizeof(quoted_title),"\"%s\"",title);
        n=snprintf(out+at,cap-at,"%s{\"pid\":%d,\"ppid\":%d,\"name\":%s,\"state\":\"%s\",\"uid\":%u,\"titleId\":%s,\"appType\":null,"
            "\"authId\":null,\"rssBytes\":%llu,\"vmBytes\":%llu,\"threads\":%d,\"startedAt\":%lld,\"cpuMs\":%llu,\"control\":%s}",
            count?",":"",i32(p+KI_PID),i32(p+KI_PPID),name,state_name(p[KI_STAT]),(unsigned)i32(p+KI_UID),quoted_title,
            (unsigned long long)(u64(p+KI_RSSIZE)*page),(unsigned long long)u64(p+KI_SIZE),i32(p+KI_THREADS),
            (long long)u64(p+KI_START),(unsigned long long)(u64(p+KI_RUNTIME)/1000),kind);
        if (n<0 || (size_t)n>=cap-at-64) { truncated=true; break; }
        at+=(size_t)n; count++;
    }
    free(control); free(table);
    n=snprintf(out+at,cap-at,"],\"truncated\":%s,\"pageBytes\":%llu}",truncated?"true":"false",(unsigned long long)page);
    return n<0 || (size_t)n>=cap-at ? -1 : 0;
}

/* 1 while `pid` is listed and not a zombie, 0 once it has exited. */
static int process_alive(int pid) {
    size_t length=0; uint8_t *table=process_table(&length); if (!table) return 1;
    int alive=0;
    for (size_t offset=0;record_valid(table,length,offset);offset+=KINFO_SIZE)
        if (i32(table+offset+KI_PID)==pid) { alive=table[offset+KI_STAT]!=5; break; } /* 5: SZOMB */
    free(table); return alive;
}

int ps4_process_control(const uint8_t *body, size_t n, char *out, size_t cap, char *error, size_t error_cap) {
    int pid; char action, expected[PC_NAME_MAX];
    if (pc_parse(body,n,&pid,&action,expected)) { snprintf(error,error_cap,"Invalid stop request."); return -1; }
    size_t length=0; uint8_t *table=process_table(&length);
    if (!table || !record_valid(table,length,0)) { free(table); snprintf(error,error_cap,"The process list could not be read."); return -1; }
    size_t count=0; PcProcess *items=control_table(table,length,&count); free(table);
    if (!items) { snprintf(error,error_cap,"Not enough memory to read the process list."); return -1; }
    PcKind kind=pc_classify(items,count,own_pid(),pid,error,error_cap);
    const PcProcess *target=NULL;
    for (size_t i=0;i<count;i++) if (items[i].pid==pid) target=&items[i];
    if (kind!=PC_NONE && target && strcmp(target->name,expected)) { snprintf(error,error_cap,"That process ID now belongs to %s. Refresh the list.",target->name); kind=PC_NONE; }
    uint32_t app_id=target ? target->app_id : 0;
    free(items);
    if (kind==PC_NONE) return -1;
    PcMethod method=pc_method(kind,action,app_id);
    KillAppFn kill_app=(KillAppFn)symbol("sceSystemServiceKillApp");
    KillFn kill_fn=(KillFn)symbol("kill");
    int rc=-1;
    if (method==PC_KILL_APP) { rc=kill_app ? kill_app(app_id,-1,0,0) : -1; if (rc) method=PC_SIGTERM; }
    if (method!=PC_KILL_APP) {
        if (!kill_fn) { snprintf(error,error_cap,"This PS4 does not expose a way to stop processes to the receiver."); return -1; }
        rc=kill_fn(pid,method==PC_SIGKILL?9:15);
        if (rc && errno!=3) { snprintf(error,error_cap,"The console refused to stop %s (error %d).",expected,errno); return -1; } /* 3: ESRCH */
    }
    uint64_t started=dx_platform_ms(), limit=action=='e' ? 2000 : 5000;
    bool exited=false;
    while (!(exited=!process_alive(pid)) && dx_platform_ms()-started<limit) rx_sleep(100);
    char quoted[64]; dx_json_string(quoted,sizeof(quoted),expected,sizeof(expected));
    int written=snprintf(out,cap,"{\"pid\":%d,\"name\":%s,\"kind\":\"%s\",\"method\":\"%s\",\"exited\":%s,\"waitedMs\":%llu}",
        pid,quoted,pc_kind_name(kind),pc_method_name(method),exited?"true":"false",(unsigned long long)(dx_platform_ms()-started));
    if (written<0 || (size_t)written>=cap) { snprintf(error,error_cap,"Reply overflow."); return -1; }
    return 0;
}

void ps4_process_summary(unsigned *count, char running[10]) {
    *count=0; running[0]=0;
    size_t length=0; uint8_t *table=process_table(&length); if (!table) return;
    for (size_t offset=0;record_valid(table,length,offset);offset+=KINFO_SIZE) {
        (*count)++;
        if (!running[0]) {
            char title[10]; app_title(i32(table+offset+KI_PID),title);
            if (!memcmp(title,"CUSA",4)) memcpy(running,title,10);
        }
    }
    free(table);
}

/* ------------------------------------------------------------------ system values */

bool ps4_temperatures(int *cpu, int *soc) {
    TemperatureFn cpu_fn=(TemperatureFn)symbol("sceKernelGetCpuTemperature");
    SensorFn soc_fn=(SensorFn)symbol("sceKernelGetSocSensorTemperature");
    *cpu=*soc=-1; int value=0;
    if (cpu_fn && !cpu_fn(&value) && value>0 && value<150) *cpu=value;
    if (soc_fn && !soc_fn(0,&value) && value>0 && value<150) *soc=value;
    return *cpu>=0 || *soc>=0;
}
long ps4_cpu_mhz(void) {
    FrequencyFn fn=(FrequencyFn)symbol("sceKernelGetCpuFrequency");
    long hz=fn ? fn() : 0; return hz>0 ? hz/1000000 : 0;
}
bool ps4_memory(uint64_t *total, uint64_t *free_bytes) {
    uint64_t physical=0,pages=0,page=0;
    if (!sysctl_u64("hw.physmem",&physical) || !sysctl_u64("vm.stats.vm.v_free_count",&pages) || !sysctl_u64("vm.stats.vm.v_page_size",&page)) return false;
    if (!physical || !page || pages>physical/page) return false;
    *total=physical; *free_bytes=pages*page; return true;
}
bool ps4_uptime(uint64_t *seconds) {
    int64_t boot[2]={0,0}; size_t size=sizeof(boot);
    if (!sysctl_name("kern.boottime",boot,&size) || size!=sizeof(boot) || boot[0]<=0) return false;
    uint64_t now=rx_wall_time();
    if (now<(uint64_t)boot[0]) return false;
    *seconds=now-(uint64_t)boot[0]; return true;
}
bool ps4_host_name(char *out, size_t cap) {
    HostNameFn fn=(HostNameFn)symbol("gethostname");
    char name[128]={0};
    if (!fn || fn(name,sizeof(name)-1) || !name[0]) return false;
    dx_json_string(out,cap,name,sizeof(name)); return true;
}
void ps4_network(char ip[20], char mac[20], char interface_name[20]) {
    ip[0]=mac[0]=interface_name[0]=0;
    IfaddrsFn get=(IfaddrsFn)symbol("getifaddrs"); FreeIfaddrsFn release=(FreeIfaddrsFn)symbol("freeifaddrs");
    RawIfaddrs *list=NULL; if (!get || !release || get(&list)) return;
    for (RawIfaddrs *i=list;i;i=i->next) {
        /* sockaddr_in: len, family (AF_INET 2), port, then the IPv4 address. */
        if (!i->address || i->address[1]!=2 || !i->name || !strncmp(i->name,"lo",2)) continue;
        const uint8_t *a=i->address+4;
        size_t length=0;
        while (length<15 && i->name[length]>' ' && i->name[length]<127 && i->name[length]!='"' && i->name[length]!='\\') { interface_name[length]=i->name[length]; length++; }
        interface_name[length]=0;
        if (!length) continue;
        snprintf(ip,20,"%u.%u.%u.%u",a[0],a[1],a[2],a[3]);
        break;
    }
    for (RawIfaddrs *i=list;i && interface_name[0];i=i->next) {
        /* sockaddr_dl: len, family (AF_LINK 18), index, type, nlen, alen, slen, then name and address. */
        if (!i->address || i->address[1]!=18 || !i->name || strcmp(i->name,interface_name)) continue;
        uint8_t nlen=i->address[5], alen=i->address[6];
        if (alen!=6 || 8u+nlen+6u>i->address[0]) continue;
        const uint8_t *b=i->address+8+nlen;
        snprintf(mac,20,"%02x:%02x:%02x:%02x:%02x:%02x",b[0],b[1],b[2],b[3],b[4],b[5]);
    }
    release(list);
}

/* Native FreeBSD 9 statfs (472 bytes); the OpenOrbis header describes a later layout. */
enum { STATFS_SIZE=472, SF_FLAGS=8, SF_BSIZE=16, SF_BLOCKS=32, SF_BAVAIL=48, SF_TYPE=280, SF_FROM=296, SF_ON=384, SF_NAME=88 };
int ps4_mounts_json(char *out, size_t cap, size_t *at) {
    FsStatFn fn=(FsStatFn)symbol("getfsstat");
    int count=fn ? fn(NULL,0,2) : -1; /* MNT_NOWAIT */
    if (count<=0) { int n=snprintf(out+*at,cap-*at,"null"); if (n<0 || (size_t)n>=cap-*at) return -1; *at+=(size_t)n; return 0; }
    if (count>256) count=256;
    uint8_t *list=calloc((size_t)count,STATFS_SIZE); if (!list) return -1;
    count=fn(list,(long)count*STATFS_SIZE,2);
    int n=snprintf(out+*at,cap-*at,count<0?"null":"[");
    if (n<0 || (size_t)n>=cap-*at) { free(list); return -1; } *at+=(size_t)n;
    for (int i=0;i<count && i<160;i++) {
        const uint8_t *f=list+(size_t)i*STATFS_SIZE;
        char from[120],on[120],type[40];
        dx_json_string(from,sizeof(from),(const char *)f+SF_FROM,SF_NAME);
        dx_json_string(on,sizeof(on),(const char *)f+SF_ON,SF_NAME);
        dx_json_string(type,sizeof(type),(const char *)f+SF_TYPE,16);
        uint64_t bsize=u64(f+SF_BSIZE), blocks=u64(f+SF_BLOCKS), avail=u64(f+SF_BAVAIL), total=0, free_bytes=0;
        if (bsize && blocks<=UINT64_MAX/bsize) { total=blocks*bsize; if ((int64_t)avail>0 && avail<=blocks) free_bytes=avail*bsize; }
        n=snprintf(out+*at,cap-*at,"%s{\"from\":%s,\"on\":%s,\"type\":%s,\"readOnly\":%s,\"totalBytes\":%llu,\"freeBytes\":%llu}",i?",":"",from,on,type,
            u64(f+SF_FLAGS)&1?"true":"false",(unsigned long long)total,(unsigned long long)free_bytes);
        if (n<0 || (size_t)n>=cap-*at) { free(list); return -1; }
        *at+=(size_t)n;
    }
    free(list);
    if (count>=0) { n=snprintf(out+*at,cap-*at,"]"); if (n<0 || (size_t)n>=cap-*at) return -1; *at+=(size_t)n; }
    return 0;
}
