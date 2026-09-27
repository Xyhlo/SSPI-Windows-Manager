#include "console_tools.h"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef SSPI_CONSOLE_HOST_TEST
#include <windows.h>
#else
#include <dirent.h>
#include <sys/mount.h>
#include <sys/sysctl.h>
#include <time.h>
#include <unistd.h>
#include <ps5/kernel.h>
#endif

#define PS5_DATA_ROOT "/data/SSPI"
static const char *const app_roots[]={"/user/app","/mnt/ext0/user/app","/mnt/ext1/user/app","/system_ex/app"};
static bool ps5_library_id(const char *id) { return ct_valid_id(id) && (!memcmp(id,"PPSA",4)||!memcmp(id,"CUSA",4)); }
static uint32_t meta_be32(const uint8_t *p) { return ((uint32_t)p[0]<<24)|((uint32_t)p[1]<<16)|((uint32_t)p[2]<<8)|p[3]; }
static uint64_t meta_le64(const uint8_t *p) { uint64_t n=0; for (unsigned i=0;i<8;i++) n|=(uint64_t)p[i]<<(i*8); return n; }
static void meta_le32(uint8_t *p,uint32_t n) { for(unsigned i=0;i<4;i++) p[i]=(uint8_t)(n>>(i*8)); }
static int package_header(const char *path, uint8_t *header, uint64_t *base) {
    *base=0;
    if (ct_read_at(path,header,128,0)) return -1;
    if (!memcmp(header,"\x7f" "FIH",4)) { *base=meta_le64(header+0x58); if (*base<0x10000 || ct_read_at(path,header,128,*base)) return -1; }
    return memcmp(header,"\x7f" "CNT",4) ? -1 : 0;
}
bool ps5_title_installed(const char *id) {
    if (!ps5_library_id(id)) return false;
    for (size_t i=0;i<sizeof(app_roots)/sizeof(app_roots[0]);i++) {
        char path[512]; uint8_t header[128]; uint64_t base;
        snprintf(path,sizeof(path),"%s/%s/app.pkg",app_roots[i],id);
        if (!package_header(path,header,&base)) {
            /* PS4 CNT identity is known; PS5 CNT variants are paired with metadata below. */
            if (!memcmp(header+0x47,id,9)) return true;
            if (!memcmp(id,"PPSA",4)) {
                for (size_t j=0;j<ct_metadata_root_count;j++) {
                    snprintf(path,sizeof(path),"%s/%s/param.json",ct_metadata_roots[j],id);
                    if (ct_kind(path)==1) return true;
                }
            }
        }
        snprintf(path,sizeof(path),"%s/%s/eboot.bin",app_roots[i],id);
        if (!ct_read_at(path,header,4,0)) {
            snprintf(path,sizeof(path),"%s/%s/sce_sys/%s",app_roots[i],id,!memcmp(id,"PPSA",4)?"param.json":"param.sfo");
            if (ct_kind(path)==1) return true;
        }
    }
    return false;
}
typedef struct { char ids[2048][10]; size_t count; bool truncated; unsigned errors; } Ps5Scan;
static void ps5_candidate(Ps5Scan *scan,const char *id) {
    if (!ps5_library_id(id)) return;
    for (size_t i=0;i<scan->count;i++) if (!strcmp(scan->ids[i],id)) return;
    if (!ps5_title_installed(id)) { scan->errors++; return; }
    if (scan->count==2048) { scan->truncated=true; return; }
    memcpy(scan->ids[scan->count++],id,10);
}
static void ps5_scan(Ps5Scan *scan,const char *root,bool optional) {
    int k=ct_kind(root); if (!k && optional) return;
    if (k!=2) { scan->errors++; return; }
#ifdef SSPI_CONSOLE_HOST_TEST
    char pattern[512]; snprintf(pattern,sizeof(pattern),".%s/*",root); WIN32_FIND_DATAA entry;
    HANDLE h=FindFirstFileA(pattern,&entry);
    if (h==INVALID_HANDLE_VALUE) { if (GetLastError()!=ERROR_FILE_NOT_FOUND) scan->errors++; return; }
    do { ps5_candidate(scan,entry.cFileName); } while (FindNextFileA(h,&entry));
    if (GetLastError()!=ERROR_NO_MORE_FILES) scan->errors++;
    FindClose(h);
#else
    DIR *dir=opendir(root); if (!dir) { scan->errors++; return; }
    for (;;) { errno=0; struct dirent *entry=readdir(dir); if (!entry) { if (errno) scan->errors++; break; } ps5_candidate(scan,entry->d_name); }
    if (closedir(dir)) scan->errors++;
#endif
}
int ps5_library_json(char *out,size_t cap) {
    Ps5Scan *scan=calloc(1,sizeof(*scan)); if (!scan) return -1;
    for (size_t i=0;i<sizeof(app_roots)/sizeof(app_roots[0]);i++) ps5_scan(scan,app_roots[i],i!=0);
    size_t at=0;
#define ADD(...) do { int n=snprintf(out+at,cap-at,__VA_ARGS__); if(n<0 || (size_t)n>=cap-at) { free(scan); return -1; } at+=(size_t)n; } while(0)
    ADD("{\"titles\":[");
    for (size_t i=0;i<scan->count;i++) ADD("%s\"%s\"",i?",":"",scan->ids[i]);
    ADD("],\"complete\":%s,\"truncated\":%s,\"errorsTruncated\":false,\"errors\":[%s],\"customIcons\":[",
        !scan->errors&&!scan->truncated?"true":"false",scan->truncated?"true":"false",
        scan->errors?"\"Some installed applications or storage directories could not be read.\"":"");
    unsigned custom=0;
    for (size_t i=0;i<scan->count;i++) if (ct_custom_icon(PS5_DATA_ROOT,scan->ids[i])) ADD("%s\"%s\"",custom++?",":"",scan->ids[i]);
    ADD("]}");
#undef ADD
    free(scan); return 0;
}
static int pkg_entry(const char *path,const char *id,uint32_t entry,uint8_t *out,size_t cap,size_t *size) {
    uint8_t h[128]; uint64_t base; if (package_header(path,h,&base) || memcmp(h+0x47,id,9)) return -1;
    uint32_t count=meta_be32(h+0x10), table=meta_be32(h+0x18);
    if (count>65536) return -1;
    for (uint32_t i=0;i<count;i++) {
        uint8_t raw[32]; if (ct_read_at(path,raw,sizeof(raw),base+table+(uint64_t)i*32)) return -1;
        if (meta_be32(raw)!=entry || (meta_be32(raw+8)&0x80000000u)) continue;
        size_t n=meta_be32(raw+0x14); if (!n || n>cap || ct_read_at(path,out,n,base+meta_be32(raw+0x10))) return -1;
        *size=n; return 0;
    }
    return -1;
}
static int ps5_read_meta(const char *id,bool patch,uint8_t *out,size_t *size) {
    bool ppsa=!memcmp(id,"PPSA",4); char path[512];
    const char *patch_roots[]={"/user/patch","/mnt/ext0/user/patch","/mnt/ext1/user/patch"};
    size_t roots=patch ? sizeof(patch_roots)/sizeof(patch_roots[0]) : sizeof(app_roots)/sizeof(app_roots[0]);
    for (size_t i=0;i<roots;i++) {
        const char *root=patch ? patch_roots[i] : app_roots[i];
        snprintf(path,sizeof(path),"%s/%s/sce_sys/%s",root,id,ppsa?"param.json":"param.sfo");
        if (!ct_read(path,out,CT_MAX_META,size) && *size) return 0;
        if (!ppsa) {
            snprintf(path,sizeof(path),"%s/%s/%s.pkg",root,id,patch?"patch":"app");
            if (!pkg_entry(path,id,0x1000,out,CT_MAX_META,size)) return 0;
        }
    }
    if (!patch) for (size_t i=0;i<ct_metadata_root_count;i++) {
        snprintf(path,sizeof(path),"%s/%s/%s",ct_metadata_roots[i],id,ppsa?"param.json":"param.sfo");
        if (!ct_read(path,out,CT_MAX_META,size) && *size) return 0;
    }
    return -1;
}
int ps5_metadata(const char *id,uint8_t *out,size_t *size) {
    if (!ps5_title_installed(id)) return -1;
    size_t at=0;
    for (unsigned field=0;field<3;field++) {
        size_t n=0;
        if (field<2) { if (ps5_read_meta(id,field==1,out+at+4,&n)) n=0; }
        else if (ct_icon_get(PS5_DATA_ROOT,id,false,out+at+4,&n)) n=0;
        meta_le32(out+at,(uint32_t)n); at+=n+4;
    }
    *size=at; return 0;
}
#ifndef SSPI_CONSOLE_HOST_TEST
int ps5_system_info(char *out,size_t cap) {
    char firmware[32]="null", raw[32]="\"unavailable\"", uptime[32]="null", name[256]="null", hostname[128]={0};
    uint32_t fw=kernel_get_fw_version();
    if (fw && fw!=UINT32_MAX) { snprintf(firmware,sizeof(firmware),"\"%x.%02x\"",fw>>24,(fw>>16)&255); snprintf(raw,sizeof(raw),"\"0x%08x\"",fw); }
    struct timespec up;
    if (!clock_gettime(CLOCK_UPTIME,&up) && up.tv_sec>=0) snprintf(uptime,sizeof(uptime),"%llu",(unsigned long long)up.tv_sec);
    if (!gethostname(hostname,sizeof(hostname)-1) && hostname[0]) ct_json_quote(name,sizeof(name),hostname);
    char storage[1600]=""; size_t at=0; const char *paths[]={"/user","/mnt/ext0","/mnt/ext1"};
    for (size_t i=0;i<3;i++) {
        struct statfs fs; if (ct_kind(paths[i])!=2 || statfs(paths[i],&fs) || !fs.f_blocks || !fs.f_bsize || fs.f_bavail<0) continue;
        if ((uint64_t)fs.f_blocks>UINT64_MAX/(uint64_t)fs.f_bsize || (uint64_t)fs.f_bavail>(uint64_t)fs.f_blocks) continue;
        int n=snprintf(storage+at,sizeof(storage)-at,"%s{\"label\":\"%s\",\"path\":\"%s\",\"totalBytes\":%llu,\"freeBytes\":%llu}",at?",":"",i==0?"Internal":i==1?"Extended 0":"Extended 1",paths[i],(unsigned long long)fs.f_blocks*fs.f_bsize,(unsigned long long)fs.f_bavail*fs.f_bsize);
        if (n<0 || (size_t)n>=sizeof(storage)-at) return -1; at+=(size_t)n;
    }
    int n=snprintf(out,cap,"{\"target\":\"ps5\",\"receiverVersion\":\"1.0.6\",\"firmware\":%s,\"sdkVersion\":null,\"model\":null,\"consoleName\":%s,\"uptimeSeconds\":%s,\"cpuTempC\":null,\"socTempC\":null,\"storage\":[%s],\"memory\":null,\"network\":null,\"runningTitleId\":null,\"capabilities\":[" PS5_CONSOLE_CAPS "],\"extras\":{\"firmwareRaw\":%s}}",firmware,name,uptime,storage,raw);
    return n<0 || (size_t)n>=cap ? -1 : 0;
}
#endif
