/* PS5 system information, processes and the platform hooks behind diagnostics.c.
   Everything here is read-only; values the console refuses are reported as null. */
/* net/if_dl.h relies on u_char from sys/types.h being declared first. */
#include <sys/types.h>
#include "console_tools.h"
#include "diagnostics.h"
#include "process_control.h"
#include <arpa/inet.h>
#include <signal.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <ifaddrs.h>
#include <net/if_dl.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/param.h>
#include <sys/proc.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/time.h>
#include <sys/user.h>
#include <time.h>
#include <unistd.h>
#include <ps5/kernel.h>

int sceKernelGetHwModelName(char *);
long sceKernelGetCpuFrequency(void);
int sceKernelGetCpuTemperature(int *);
int sceKernelGetSocSensorTemperature(int, int *);
typedef struct { uint32_t app_id; uint64_t unknown1; uint32_t app_type; char title_id[10]; char unknown2[0x3c]; } Ps5AppInfo;
int sceKernelGetAppInfo(pid_t pid, Ps5AppInfo *info);
int sceSystemServiceKillApp(uint32_t app_id, int32_t option, int32_t method, int32_t reason);

/* ------------------------------------------------------------------ diagnostics hooks */

static pthread_mutex_t diagnostics_lock=PTHREAD_MUTEX_INITIALIZER;
void dx_platform_lock(void) { pthread_mutex_lock(&diagnostics_lock); }
void dx_platform_unlock(void) { pthread_mutex_unlock(&diagnostics_lock); }
uint64_t dx_platform_ms(void) {
    struct timespec ts; if (clock_gettime(CLOCK_MONOTONIC,&ts)) return 0;
    return (uint64_t)ts.tv_sec*1000+(uint64_t)ts.tv_nsec/1000000;
}
int dx_platform_list(const char *dir, DxVisit visit, void *context) {
    DIR *d=opendir(dir); if (!d) return -1;
    struct dirent *entry;
    while ((entry=readdir(d))) if (visit(entry->d_name,context)) break;
    return closedir(d) ? -1 : 0;
}
int dx_platform_lstat(const char *path, uint64_t *size, int64_t *modified) {
    struct stat st;
    if (lstat(path,&st)) return errno==ENOENT || errno==ENOTDIR ? 0 : -1;
    if (S_ISDIR(st.st_mode)) return 2;
    if (!S_ISREG(st.st_mode) || st.st_size<0) return -1;
    if (size) *size=(uint64_t)st.st_size;
    if (modified) *modified=(int64_t)st.st_mtime;
    return 1;
}
int dx_platform_msgbuf(char *out, size_t cap, size_t *size, int *error) {
    size_t length=0; *size=0; *error=0;
    if (sysctlbyname("kern.msgbuf",NULL,&length,NULL,0)) { *error=errno; return -1; }
    if (!length || length>64u*1024u*1024u) { *error=EIO; return -1; }
    length+=4096;
    char *buffer=malloc(length); if (!buffer) { *error=ENOMEM; return -1; }
    if (sysctlbyname("kern.msgbuf",buffer,&length,NULL,0)) { *error=errno; free(buffer); return -1; }
    while (length && !buffer[length-1]) length--;
    if (!length) { *error=EIO; free(buffer); return -1; }
    size_t take=length<cap ? length : cap;
    memcpy(out,buffer+length-take,take); free(buffer);
    *size=take; return 0;
}
int dx_platform_klog_drain(char *out, size_t cap, size_t *size, int *error) {
    *size=0; *error=0;
    int fd=open("/dev/klog",O_RDONLY|O_NONBLOCK);
    if (fd<0) { *error=errno; return *error==EBUSY ? -2 : -1; }
    size_t n=0;
    while (n<cap) {
        ssize_t got=read(fd,out+n,cap-n);
        if (got>0) { n+=(size_t)got; continue; }
        if (got<0 && errno==EINTR) continue;
        break;
    }
    close(fd); *size=n; return 0;
}

/* ------------------------------------------------------------------ processes */

static const char *state_name(char state) {
    static const char *const names[]={"unknown","starting","running","sleeping","stopped","zombie","waiting","locked"};
    return state>0 && state<8 ? names[(int)state] : names[0];
}
static bool game_title(const char *id) {
    return (ct_ps4_title_id(id) || !memcmp(id,"PPSA",4)) && ct_valid_id(id);
}
static void *process_table(size_t *size) {
    int mib[4]={CTL_KERN,KERN_PROC,KERN_PROC_PROC,0};
    size_t length=0;
    if (sysctl(mib,4,NULL,&length,NULL,0) || !length) return NULL;
    for (unsigned attempt=0;attempt<3;attempt++) {
        length+=length/4+sizeof(struct kinfo_proc)*16;
        void *table=malloc(length); if (!table) return NULL;
        size_t got=length;
        if (!sysctl(mib,4,table,&got,NULL,0)) { *size=got; return table; }
        free(table);
        if (errno!=ENOMEM) return NULL;
    }
    return NULL;
}
static void app_info(pid_t pid, char title[10], uint32_t *type, uint32_t *app_id) {
    Ps5AppInfo info; memset(&info,0,sizeof(info)); title[0]=0; *type=0; if (app_id) *app_id=0;
    if (sceKernelGetAppInfo(pid,&info)) return;
    memcpy(title,info.title_id,9); title[9]=0;
    if (!ct_valid_id(title)) title[0]=0;
    *type=info.app_type; if (app_id) *app_id=info.app_id;
}
static void app_title(pid_t pid, char title[10], uint32_t *type) { app_info(pid,title,type,NULL); }

/* The process table as the stop rules see it. Auth IDs cost kernel reads, so only the
   loader and orphans (whose auth ID decides whether they are payloads) are read. */
static PcProcess *control_table(const char *table, size_t length, size_t *count) {
    size_t capacity=length/sizeof(struct kinfo_proc)+1, n=0;
    PcProcess *items=calloc(capacity,sizeof(PcProcess)); if (!items) return NULL;
    for (size_t offset=0;offset+sizeof(struct kinfo_proc)<=length && n<capacity;) {
        const struct kinfo_proc *p=(const struct kinfo_proc *)(table+offset);
        if (p->ki_structsize<(int)sizeof(struct kinfo_proc)) break;
        offset+=(size_t)p->ki_structsize;
        PcProcess *item=&items[n++]; uint32_t type=0;
        item->pid=p->ki_pid; item->ppid=p->ki_ppid;
        size_t name_length=strnlen(p->ki_comm,sizeof(p->ki_comm)); if (name_length>=PC_NAME_MAX) name_length=PC_NAME_MAX-1;
        memcpy(item->name,p->ki_comm,name_length); item->name[name_length]=0;
        app_info(p->ki_pid,item->title,&type,&item->app_id);
        if (p->ki_ppid==1 || !strcmp(item->name,"elfldr.elf")) item->authid=kernel_get_ucred_authid(p->ki_pid);
    }
    *count=n; return items;
}

int ps5_processes_json(char *out, size_t cap) {
    size_t length=0; char *table=process_table(&length);
    if (!table) return -1;
    size_t controls=0; PcProcess *control=control_table(table,length,&controls);
    if (!control) { free(table); return -1; }
    int self=getpid();
    size_t at=0; unsigned count=0; bool truncated=false;
    uint64_t authid_until=dx_platform_ms()+600;
    int n=snprintf(out,cap,"{\"processes\":["); if (n<0 || (size_t)n>=cap) { free(table); return -1; }
    at=(size_t)n;
    for (size_t offset=0;offset+sizeof(struct kinfo_proc)<=length;) {
        struct kinfo_proc *p=(struct kinfo_proc *)(table+offset);
        if (p->ki_structsize<(int)sizeof(struct kinfo_proc)) break;
        offset+=(size_t)p->ki_structsize;
        char name[64],title[10],quoted_title[16]="null",authid[24]="null",kind[16]="null"; uint32_t type=0;
        const char *can=pc_kind_name(pc_classify(control,controls,self,p->ki_pid,NULL,0));
        if (can) snprintf(kind,sizeof(kind),"\"%s\"",can);
        dx_json_string(name,sizeof(name),p->ki_comm,sizeof(p->ki_comm));
        app_title(p->ki_pid,title,&type);
        if (title[0]) snprintf(quoted_title,sizeof(quoted_title),"\"%s\"",title);
        /* Each auth ID costs kernel reads; stop asking once the budget is spent. */
        if (dx_platform_ms()<authid_until) {
            uint64_t id=kernel_get_ucred_authid(p->ki_pid);
            if (id) snprintf(authid,sizeof(authid),"\"%016llx\"",(unsigned long long)id);
        }
        n=snprintf(out+at,cap-at,"%s{\"pid\":%d,\"ppid\":%d,\"name\":%s,\"state\":\"%s\",\"uid\":%u,\"titleId\":%s,\"appType\":%u,"
            "\"authId\":%s,\"rssBytes\":%llu,\"vmBytes\":%llu,\"threads\":%d,\"startedAt\":%lld,\"cpuMs\":%llu,\"control\":%s}",
            count?",":"",p->ki_pid,p->ki_ppid,name,state_name(p->ki_stat),(unsigned)p->ki_uid,quoted_title,type,authid,
            (unsigned long long)p->ki_rssize*PAGE_SIZE,(unsigned long long)p->ki_size,p->ki_numthreads,
            (long long)p->ki_start.tv_sec,(unsigned long long)(p->ki_runtime/1000),kind);
        if (n<0 || (size_t)n>=cap-at-64) { truncated=true; break; }
        at+=(size_t)n; count++;
    }
    free(control); free(table);
    n=snprintf(out+at,cap-at,"],\"truncated\":%s,\"pageBytes\":%d}",truncated?"true":"false",(int)PAGE_SIZE);
    return n<0 || (size_t)n>=cap-at ? -1 : 0;
}

/* ------------------------------------------------------------------ stopping processes */

/* 1 while `pid` exists and is not a zombie, 0 once it has exited. */
static int process_alive(pid_t pid) {
    int mib[4]={CTL_KERN,KERN_PROC,KERN_PROC_PID,(int)pid};
    struct kinfo_proc info; size_t size=sizeof(info);
    if (sysctl(mib,4,&info,&size,NULL,0) || size<sizeof(info)) return 0;
    return info.ki_stat!=SZOMB;
}

int ps5_process_control(const uint8_t *body, size_t n, char *out, size_t cap, char *error, size_t error_cap) {
    int pid; char action, expected[PC_NAME_MAX];
    if (pc_parse(body,n,&pid,&action,expected)) { snprintf(error,error_cap,"Invalid stop request."); return -1; }
    size_t length=0; char *table=process_table(&length);
    if (!table) { snprintf(error,error_cap,"The process list could not be read."); return -1; }
    size_t count=0; PcProcess *items=control_table(table,length,&count); free(table);
    if (!items) { snprintf(error,error_cap,"Not enough memory to read the process list."); return -1; }
    PcKind kind=pc_classify(items,count,getpid(),pid,error,error_cap);
    const PcProcess *target=NULL;
    for (size_t i=0;i<count;i++) if (items[i].pid==pid) target=&items[i];
    if (kind!=PC_NONE && target && strcmp(target->name,expected)) { snprintf(error,error_cap,"That process ID now belongs to %s. Refresh the list.",target->name); kind=PC_NONE; }
    uint32_t app_id=target ? target->app_id : 0;
    free(items);
    if (kind==PC_NONE) return -1;
    PcMethod method=pc_method(kind,action,app_id);
    int rc=0;
    if (method==PC_KILL_APP) { rc=sceSystemServiceKillApp(app_id,-1,0,0); if (rc) { method=PC_SIGTERM; rc=0; } }
    if (method!=PC_KILL_APP) rc=kill(pid,method==PC_SIGKILL?SIGKILL:SIGTERM);
    if (rc && errno!=ESRCH) { snprintf(error,error_cap,"The console refused to stop %s (error %d).",expected,errno); return -1; }
    /* Report whether it actually went away; the interface animates on this. */
    uint64_t started=dx_platform_ms(), limit=action=='e' ? 2000 : 5000;
    bool exited=false;
    while (!(exited=!process_alive(pid)) && dx_platform_ms()-started<limit) usleep(100000);
    char quoted[64]; dx_json_string(quoted,sizeof(quoted),expected,sizeof(expected));
    int written=snprintf(out,cap,"{\"pid\":%d,\"name\":%s,\"kind\":\"%s\",\"method\":\"%s\",\"exited\":%s,\"waitedMs\":%llu}",
        pid,quoted,pc_kind_name(kind),pc_method_name(method),exited?"true":"false",(unsigned long long)(dx_platform_ms()-started));
    if (written<0 || (size_t)written>=cap) { snprintf(error,error_cap,"Reply overflow."); return -1; }
    return 0;
}

/* Counts processes and finds the foreground game, if any. */
static void process_summary(unsigned *count, char running[10]) {
    *count=0; running[0]=0;
    size_t length=0; char *table=process_table(&length); if (!table) return;
    for (size_t offset=0;offset+sizeof(struct kinfo_proc)<=length;) {
        struct kinfo_proc *p=(struct kinfo_proc *)(table+offset);
        if (p->ki_structsize<(int)sizeof(struct kinfo_proc)) break;
        offset+=(size_t)p->ki_structsize; (*count)++;
        if (!running[0]) { char title[10]; uint32_t type; app_title(p->ki_pid,title,&type); if (game_title(title)) memcpy(running,title,10); }
    }
    free(table);
}

/* ------------------------------------------------------------------ system information */

static int add(char *out, size_t cap, size_t *at, const char *format, ...) __attribute__((format(printf,4,5)));
static int add(char *out, size_t cap, size_t *at, const char *format, ...) {
    va_list args; va_start(args,format);
    int n=vsnprintf(out+*at,cap-*at,format,args); va_end(args);
    if (n<0 || (size_t)n>=cap-*at) return -1;
    *at+=(size_t)n; return 0;
}
static bool sysctl_u64(const char *name, uint64_t *value) {
    uint64_t wide=0; size_t size=sizeof(wide);
    if (sysctlbyname(name,&wide,&size,NULL,0)) return false;
    if (size==sizeof(uint32_t)) { uint32_t narrow; memcpy(&narrow,&wide,sizeof(narrow)); *value=narrow; }
    else if (size==sizeof(uint64_t)) *value=wide;
    else return false;
    return true;
}
static int storage_json(char *out, size_t cap, size_t *at) {
    struct { const char *path, *label; } volumes[]={{"/user","Internal"},{"/mnt/ext0","Extended 0"},{"/mnt/ext1","Extended 1"},
        {"/mnt/usb0","USB 0"},{"/mnt/usb1","USB 1"},{"/mnt/usb2","USB 2"},{"/mnt/usb3","USB 3"},
        {"/mnt/usb4","USB 4"},{"/mnt/usb5","USB 5"},{"/mnt/usb6","USB 6"},{"/mnt/usb7","USB 7"}};
    unsigned count=0;
    for (size_t i=0;i<sizeof(volumes)/sizeof(volumes[0]);i++) {
        struct statfs fs;
        if (ct_kind(volumes[i].path)!=2 || statfs(volumes[i].path,&fs) || !fs.f_blocks || !fs.f_bsize || fs.f_bavail<0) continue;
        if ((uint64_t)fs.f_blocks>UINT64_MAX/(uint64_t)fs.f_bsize || (uint64_t)fs.f_bavail>(uint64_t)fs.f_blocks) continue;
        /* USB folders exist even when nothing is plugged in; only real mount points count. */
        if (i>=3 && strcmp(fs.f_mntonname,volumes[i].path)) continue;
        if (add(out,cap,at,"%s{\"label\":\"%s\",\"path\":\"%s\",\"totalBytes\":%llu,\"freeBytes\":%llu}",count++?",":"",volumes[i].label,volumes[i].path,
            (unsigned long long)fs.f_blocks*fs.f_bsize,(unsigned long long)fs.f_bavail*fs.f_bsize)) return -1;
    }
    return 0;
}
static int mounts_json(char *out, size_t cap, size_t *at) {
    int count=getfsstat(NULL,0,MNT_NOWAIT);
    if (count<=0) return add(out,cap,at,"null");
    if (count>256) count=256;
    struct statfs *list=calloc((size_t)count,sizeof(*list));
    if (!list) return add(out,cap,at,"null");
    count=getfsstat(list,(long)((size_t)count*sizeof(*list)),MNT_NOWAIT);
    if (count<0) { free(list); return add(out,cap,at,"null"); }
    if (add(out,cap,at,"[")) { free(list); return -1; }
    for (int i=0;i<count && i<160;i++) {
        char from[120],on[120],type[40];
        dx_json_string(from,sizeof(from),list[i].f_mntfromname,sizeof(list[i].f_mntfromname));
        dx_json_string(on,sizeof(on),list[i].f_mntonname,sizeof(list[i].f_mntonname));
        dx_json_string(type,sizeof(type),list[i].f_fstypename,sizeof(list[i].f_fstypename));
        uint64_t total=0,free_bytes=0;
        if (list[i].f_bsize && (uint64_t)list[i].f_blocks<=UINT64_MAX/(uint64_t)list[i].f_bsize) {
            total=(uint64_t)list[i].f_blocks*list[i].f_bsize;
            free_bytes=list[i].f_bavail>0 && (uint64_t)list[i].f_bavail<=(uint64_t)list[i].f_blocks ? (uint64_t)list[i].f_bavail*list[i].f_bsize : 0;
        }
        if (add(out,cap,at,"%s{\"from\":%s,\"on\":%s,\"type\":%s,\"readOnly\":%s,\"totalBytes\":%llu,\"freeBytes\":%llu}",i?",":"",from,on,type,
            list[i].f_flags&MNT_RDONLY?"true":"false",(unsigned long long)total,(unsigned long long)free_bytes)) { free(list); return -1; }
    }
    free(list);
    return add(out,cap,at,"]");
}
static void network(char ip[64], char mac[32], char interface_name[32]) {
    ip[0]=mac[0]=interface_name[0]=0;
    struct ifaddrs *list=NULL; if (getifaddrs(&list)) return;
    for (struct ifaddrs *i=list;i;i=i->ifa_next) {
        if (!i->ifa_addr || i->ifa_addr->sa_family!=AF_INET || !i->ifa_name || !strncmp(i->ifa_name,"lo",2)) continue;
        struct sockaddr_in *address=(struct sockaddr_in *)i->ifa_addr;
        if (!inet_ntop(AF_INET,&address->sin_addr,ip,64)) { ip[0]=0; continue; }
        snprintf(interface_name,32,"%s",i->ifa_name);
        break;
    }
    for (struct ifaddrs *i=list;i && interface_name[0];i=i->ifa_next) {
        if (!i->ifa_addr || i->ifa_addr->sa_family!=AF_LINK || !i->ifa_name || strcmp(i->ifa_name,interface_name)) continue;
        struct sockaddr_dl *link=(struct sockaddr_dl *)i->ifa_addr;
        if (link->sdl_alen!=6) continue;
        const unsigned char *b=(const unsigned char *)LLADDR(link);
        snprintf(mac,32,"%02x:%02x:%02x:%02x:%02x:%02x",b[0],b[1],b[2],b[3],b[4],b[5]);
    }
    freeifaddrs(list);
}

int ps5_system_info(char *out, size_t cap) {
    char firmware[32]="null", raw[32]="\"unavailable\"", uptime[32]="null", name[256]="null", hostname[128]={0};
    uint32_t fw=kernel_get_fw_version();
    if (fw && fw!=UINT32_MAX) { snprintf(firmware,sizeof(firmware),"\"%x.%02x\"",fw>>24,(fw>>16)&255); snprintf(raw,sizeof(raw),"\"0x%08x\"",fw); }
    struct timespec up;
    if (!clock_gettime(CLOCK_UPTIME,&up) && up.tv_sec>=0) snprintf(uptime,sizeof(uptime),"%llu",(unsigned long long)up.tv_sec);
    if (!gethostname(hostname,sizeof(hostname)-1) && hostname[0]) dx_json_string(name,sizeof(name),hostname,sizeof(hostname));
    char model_text[1024]={0}, model[80]="null";
    if (!sceKernelGetHwModelName(model_text) && model_text[0]) dx_json_string(model,sizeof(model),model_text,64);
    int temperature=0; char cpu[16]="null", soc[16]="null";
    if (!sceKernelGetCpuTemperature(&temperature) && temperature>0 && temperature<150) snprintf(cpu,sizeof(cpu),"%d",temperature);
    if (!sceKernelGetSocSensorTemperature(0,&temperature) && temperature>0 && temperature<150) snprintf(soc,sizeof(soc),"%d",temperature);
    uint64_t physical=0, free_pages=0, page=0; char memory[96]="null";
    if (sysctl_u64("hw.physmem",&physical) && sysctl_u64("vm.stats.vm.v_free_count",&free_pages) && sysctl_u64("vm.stats.vm.v_page_size",&page) &&
        physical && page && free_pages<=physical/page)
        snprintf(memory,sizeof(memory),"{\"totalBytes\":%llu,\"freeBytes\":%llu}",(unsigned long long)physical,(unsigned long long)(free_pages*page));
    char ip[64],mac[32],interface_name[32],net[160]="null";
    network(ip,mac,interface_name);
    if (ip[0]) snprintf(net,sizeof(net),"{\"ip\":\"%s\",\"mac\":%s%s%s}",ip,mac[0]?"\"":"",mac[0]?mac:"null",mac[0]?"\"":"");
    unsigned processes=0; char running[10], running_json[16]="null";
    process_summary(&processes,running);
    if (running[0]) snprintf(running_json,sizeof(running_json),"\"%s\"",running);
    double load[3]={0}; char load_text[64]="";
    if (getloadavg(load,3)==3) snprintf(load_text,sizeof(load_text),",\"loadAverage\":\"%.2f %.2f %.2f\"",load[0],load[1],load[2]);
    long frequency=sceKernelGetCpuFrequency();
    size_t at=0;
    if (add(out,cap,&at,"{\"target\":\"ps5\",\"receiverVersion\":\"" PS5_RECEIVER_VERSION "\",\"firmware\":%s,\"sdkVersion\":null,\"model\":%s,"
        "\"consoleName\":%s,\"uptimeSeconds\":%s,\"cpuTempC\":%s,\"socTempC\":%s,\"storage\":[",firmware,model,name,uptime,cpu,soc)) return -1;
    if (storage_json(out,cap,&at)) return -1;
    if (add(out,cap,&at,"],\"memory\":%s,\"network\":%s,\"runningTitleId\":%s,\"mounts\":",memory,net,running_json)) return -1;
    if (mounts_json(out,cap,&at)) return -1;
    if (add(out,cap,&at,",\"capabilities\":[" PS5_CONSOLE_CAPS "],\"extras\":{\"firmwareRaw\":%s,\"processCount\":\"%u\"",raw,processes)) return -1;
    if (frequency>0 && add(out,cap,&at,",\"cpuFrequencyMhz\":\"%ld\"",frequency/1000000)) return -1;
    if (interface_name[0] && add(out,cap,&at,",\"networkInterface\":\"%s\"",interface_name)) return -1;
    return add(out,cap,&at,"%s}}",load_text);
}
