#include "console_tools.h"
#include "installed_library.h"
#include "json.h"
#include "../payload/console_files.h"
#include "../payload/diagnostics.h"
#include "system_ps4.h"
#include "log.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/statfs.h>
#include <sched.h>
#include <orbis/libkernel.h>

static RxMutex icon_lock=RX_MUTEX_INIT;
#define PS4_CAPABILITIES "[\"pkg-preflight\",\"pkg-install\",\"url-install\",\"parallel-upload\",\"verify\",\"title-context\",\"progress-notifications\",\"install-control\",\"stop\",\"ps4\",\"installed-library-v1\",\"title-icons-v1\",\"system-info-v1\",\"theme-install-v1\",\"themes-v1\",\"diagnostics-v1\",\"process-control-v1\",\"title-icons-v2\"]"
static int system_info(int fd) {
    const size_t cap=96u*1024u;
    char *out=malloc(cap); if (!out) return text_reply(fd,RESP_ERROR,"Not enough memory to read system information.");
    Json j; json_init(&j,out,cap);
    OrbisKernelSwVersion version; memset(&version,0,sizeof(version)); version.Size=sizeof(version);
    json_add(&j,"{\"target\":\"ps4\",\"receiverVersion\":\"" VERSION "\",\"firmware\":");
    char raw[16]="";
    if (!sceKernelGetSystemSwVersion(&version) && version.Version) {
        char fw[32]; snprintf(fw,sizeof(fw),"%x.%02x",version.Version>>24,(version.Version>>16)&255); json_quote(&j,fw);
        snprintf(raw,sizeof(raw),"0x%08x",(unsigned)version.Version);
    } else json_add(&j,"null");
    char host[160]="null"; ps4_host_name(host,sizeof(host));
    uint64_t uptime=0; bool has_uptime=ps4_uptime(&uptime);
    int cpu=-1,soc=-1; ps4_temperatures(&cpu,&soc);
    json_add(&j,",\"sdkVersion\":null,\"model\":null,\"consoleName\":%s,\"uptimeSeconds\":",host);
    if (has_uptime) json_add(&j,"%llu",(unsigned long long)uptime); else json_add(&j,"null");
    if (cpu>=0) json_add(&j,",\"cpuTempC\":%d",cpu); else json_add(&j,",\"cpuTempC\":null");
    if (soc>=0) json_add(&j,",\"socTempC\":%d",soc); else json_add(&j,",\"socTempC\":null");
    uint64_t total=0,free_bytes=0;
    if (ps4_memory(&total,&free_bytes)) json_add(&j,",\"memory\":{\"totalBytes\":%llu,\"freeBytes\":%llu}",(unsigned long long)total,(unsigned long long)free_bytes);
    else json_add(&j,",\"memory\":null");
    char ip[20],mac[20],interface_name[20]; ps4_network(ip,mac,interface_name);
    if (ip[0]) json_add(&j,",\"network\":{\"ip\":\"%s\",\"mac\":%s%s%s}",ip,mac[0]?"\"":"",mac[0]?mac:"null",mac[0]?"\"":"");
    else json_add(&j,",\"network\":null");
    unsigned processes=0; char running[10]; ps4_process_summary(&processes,running);
    if (running[0]) json_add(&j,",\"runningTitleId\":\"%s\"",running); else json_add(&j,",\"runningTitleId\":null");
    json_add(&j,",\"storage\":[");
    const char *paths[]={"/user","/mnt/ext0","/mnt/usb0","/mnt/usb1","/mnt/usb2","/mnt/usb3","/mnt/usb4","/mnt/usb5","/mnt/usb6","/mnt/usb7"};
    const char *labels[]={"Internal","Extended","USB 0","USB 1","USB 2","USB 3","USB 4","USB 5","USB 6","USB 7"};
    unsigned count=0;
    for (size_t i=0;i<sizeof(paths)/sizeof(paths[0]);i++) {
        struct statfs fs; if (ct_kind(paths[i])!=2 || statfs(paths[i],&fs) || !fs.f_bsize || !fs.f_blocks) continue;
        if ((uint64_t)fs.f_blocks>UINT64_MAX/(uint64_t)fs.f_bsize || (uint64_t)fs.f_bavail>(uint64_t)fs.f_blocks) continue;
        /* USB folders exist without a drive. The kernel fills the native FreeBSD 9 layout,
           whose mount-on name sits at byte 384, not where the OpenOrbis header puts it. */
        if (i>=2 && strncmp((const char *)&fs+384,paths[i],88)) continue;
        json_add(&j,"%s{\"label\":\"%s\",\"path\":\"%s\",\"totalBytes\":%llu,\"freeBytes\":%llu}",count++?",":"",labels[i],paths[i],(unsigned long long)fs.f_blocks*fs.f_bsize,(unsigned long long)fs.f_bavail*fs.f_bsize);
    }
    json_add(&j,"],\"mounts\":");
    if (!j.failed) { if (ps4_mounts_json(j.data,j.cap,&j.used)) j.failed=true; }
    json_add(&j,",\"capabilities\":" PS4_CAPABILITIES ",\"extras\":{\"processCount\":\"%u\"",processes);
    if (raw[0]) json_add(&j,",\"firmwareRaw\":\"%s\"",raw);
    long mhz=ps4_cpu_mhz(); if (mhz>0) json_add(&j,",\"cpuFrequencyMhz\":\"%ld\"",mhz);
    if (interface_name[0]) json_add(&j,",\"networkInterface\":\"%s\"",interface_name);
    json_add(&j,"}}");
    int rc=j.failed ? text_reply(fd,RESP_ERROR,"System information exceeded its size limit.") : text_reply(fd,RESP_DATA,out);
    free(out); return rc;
}
static int diagnostics(int fd, uint8_t cmd, const uint8_t *b, size_t n) {
    if (cmd!=CMD_LOG_READ && n) return text_reply(fd,RESP_ERROR,"This command requires an empty request.");
    size_t cap=cmd==CMD_PROCESSES ? 256u*1024u : cmd==CMD_LOG_LIST ? DX_LIST_MAX : DX_REPLY_MAX;
    char *out=malloc(cap), error[512]="Diagnostics could not be read.";
    if (!out) return text_reply(fd,RESP_ERROR,"Not enough memory to read diagnostics.");
    size_t size=0; int rc;
    if (cmd==CMD_PROCESSES) {
        rc=ps4_processes_json(out,cap);
        if (!rc) size=strlen(out);
        else snprintf(error,sizeof(error),rc==-2?"This PS4 returns process records in a layout the receiver does not read.":"The process list could not be read.");
    }
    else if (cmd==CMD_KERNEL_LOG) rc=dx_kernel_log(out,cap,&size,error,sizeof(error));
    else if (cmd==CMD_LOG_LIST) rc=dx_log_list(out,cap,&size,error,sizeof(error));
    else rc=dx_log_read(b,n,out,cap,&size,error,sizeof(error));
    if (rc && cmd==CMD_KERNEL_LOG) log_line("%s",error);
    int result=rc ? text_reply(fd,RESP_ERROR,error) : reply(fd,RESP_DATA,out,(uint32_t)size);
    free(out); return result;
}
int console_tools_request(int fd,uint8_t cmd,const uint8_t *b,size_t n) {
    if (cmd==CMD_PROCESS_CONTROL) {
        char out[512], error[200]="The process could not be stopped.";
        return ps4_process_control(b,n,out,sizeof(out),error,sizeof(error)) ? text_reply(fd,RESP_ERROR,error) : text_reply(fd,RESP_DATA,out);
    }
    if (cmd==CMD_KERNEL_LOG || cmd==CMD_PROCESSES || cmd==CMD_LOG_LIST || cmd==CMD_LOG_READ) return diagnostics(fd,cmd,b,n);
    if (cmd==CMD_SHELL_REFRESH || cmd==CMD_SYSTEM_INFO) {
        if (n) return text_reply(fd,RESP_ERROR,"This command requires an empty request.");
        return cmd==CMD_SHELL_REFRESH ? text_reply(fd,RESP_ERROR,"Restart your PS4 to see the new icons.") : system_info(fd);
    }
    if (!b || n<10 || b[9] || memchr(b,0,9) || !ct_valid_id((const char *)b)) return text_reply(fd,RESP_ERROR,"Invalid installed title ID.");
    /* Set v2: title ID, u32le PNG length, the PNG, then the DXT1 DDS for the home screen. */
    size_t png_size=cmd==CMD_TITLE_ICON_SET2 && n>=14 ? read_u32le(b+10) : 0;
    if ((cmd==CMD_TITLE_ICON_GET && (n!=11 || b[10]>1)) || (cmd==CMD_TITLE_ICON_RESTORE && n!=10) ||
        (cmd==CMD_TITLE_ICON_SET && (n<=10 || n>CT_MAX_PNG+10)) ||
        (cmd==CMD_TITLE_ICON_SET2 && (n<14 || !png_size || png_size>CT_MAX_PNG || n-14<=png_size || n-14-png_size>CT_MAX_DDS)))
        return text_reply(fd,RESP_ERROR,"Invalid icon request length.");
    if (!installed_library_title_present((const char *)b)) return text_reply(fd,RESP_ERROR,"Refresh Library to confirm this title is installed.");
    rx_lock(&icon_lock); int result;
    if (cmd==CMD_TITLE_ICON_GET) {
        uint8_t *png=malloc(CT_MAX_PNG); size_t size=0;
        if (!png) result=text_reply(fd,RESP_ERROR,"Not enough memory to read the icon.");
        else { int rc=ct_icon_get(DATA_ROOT,(const char *)b,b[10]!=0,png,&size); result=rc ? text_reply(fd,RESP_ERROR,"The title icon is unavailable.") : reply(fd,RESP_DATA,png,(uint32_t)size); free(png); }
    } else {
        char text[512]; int rc=cmd==CMD_TITLE_ICON_SET2
            ? ct_ps4_icon_change(DATA_ROOT,(const char *)b,b+14,png_size,b+14+png_size,n-14-png_size,false,text,sizeof(text))
            : ct_ps4_icon_change(DATA_ROOT,(const char *)b,b+10,cmd==CMD_TITLE_ICON_SET ? n-10 : 0,NULL,0,cmd==CMD_TITLE_ICON_RESTORE,text,sizeof(text));
        log_line("icon %s cid=%.9s png=%zu dds=%zu rc=%d %s",cmd==CMD_TITLE_ICON_RESTORE?"restore":cmd==CMD_TITLE_ICON_SET2?"set+dds":"set",(const char *)b,
            cmd==CMD_TITLE_ICON_SET2?png_size:cmd==CMD_TITLE_ICON_SET?n-10:(size_t)0,cmd==CMD_TITLE_ICON_SET2?n-14-png_size:(size_t)0,rc,text);
        result=text_reply(fd,rc?RESP_ERROR:RESP_DATA,text);
    }
    rx_unlock(&icon_lock); return result;
}
