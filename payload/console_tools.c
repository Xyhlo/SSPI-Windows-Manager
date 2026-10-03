#include "console_tools.h"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sqlite3.h>
#ifdef SSPI_CONSOLE_HOST_TEST
#include <windows.h>
#include <direct.h>
#include <io.h>
#define NATIVE_PREFIX "."
#define scan_stat _stat64
#define scan_open _open
#define scan_read _read
#define scan_write _write
#define scan_close _close
#define scan_unlink _unlink
#define scan_rmdir _rmdir
#define SCAN_READ_FLAGS (_O_RDONLY|_O_BINARY)
#define SCAN_WRITE_FLAGS (_O_WRONLY|_O_CREAT|_O_EXCL|_O_BINARY)
static SRWLOCK library_lock=SRWLOCK_INIT;
#define SCAN_LOCK() AcquireSRWLockExclusive(&library_lock)
#define SCAN_UNLOCK() ReleaseSRWLockExclusive(&library_lock)
unsigned ps5_test_budget_ms=3000;
const char *ps5_test_unreadable;
#else
#include <arpa/inet.h>
#include <netinet/in.h>
#include <dirent.h>
#include <poll.h>
#include <pthread.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/sysctl.h>
#include <time.h>
#include <unistd.h>
#include <ps5/kernel.h>
#define NATIVE_PREFIX ""
#define scan_stat stat
#define scan_open open
#define scan_read read
#define scan_write write
#define scan_close close
#define scan_unlink unlink
#define scan_rmdir rmdir
#define SCAN_READ_FLAGS (O_RDONLY|O_NOFOLLOW)
#define SCAN_WRITE_FLAGS (O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW)
static pthread_mutex_t library_lock=PTHREAD_MUTEX_INITIALIZER;
#define SCAN_LOCK() pthread_mutex_lock(&library_lock)
#define SCAN_UNLOCK() pthread_mutex_unlock(&library_lock)
#endif

#define PS5_DATA_ROOT "/data/SSPI"
static const char *const app_roots[]={"/user/app","/mnt/ext0/user/app","/mnt/ext1/user/app","/system_ex/app"};
static bool ps5_library_id(const char *id) { return ct_valid_id(id) && (!memcmp(id,"PPSA",4)||ct_ps4_title_id(id)); }
static uint32_t meta_be32(const uint8_t *p) { return ((uint32_t)p[0]<<24)|((uint32_t)p[1]<<16)|((uint32_t)p[2]<<8)|p[3]; }
static uint64_t meta_le64(const uint8_t *p) { uint64_t n=0; for (unsigned i=0;i<8;i++) n|=(uint64_t)p[i]<<(i*8); return n; }
static void meta_le32(uint8_t *p,uint32_t n) { for(unsigned i=0;i<4;i++) p[i]=(uint8_t)(n>>(i*8)); }
static int package_header(const char *path, uint8_t *header, uint64_t *base) {
    *base=0;
    if (ct_read_at(path,header,128,0)) return -1;
    if (!memcmp(header,"\x7f" "FIH",4)) { *base=meta_le64(header+0x58); if (*base<0x10000 || ct_read_at(path,header,128,*base)) return -1; }
    return memcmp(header,"\x7f" "CNT",4) ? -1 : 0;
}
static bool ps5_app_verified(const char *root,const char *id) {
    if (!ps5_library_id(id)) return false;
    {
        char path[512]; uint8_t header[128]; uint64_t base;
        snprintf(path,sizeof(path),"%s/%s/app.pkg",root,id);
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
        snprintf(path,sizeof(path),"%s/%s/eboot.bin",root,id);
        if (!ct_read_at(path,header,4,0)) {
            snprintf(path,sizeof(path),"%s/%s/sce_sys/%s",root,id,!memcmp(id,"PPSA",4)?"param.json":"param.sfo");
            if (ct_kind(path)==1) return true;
        }
    }
    return false;
}
#define SCAN_MAX_TITLES 2048
#define SCAN_MAX_ENTRIES 32768
#define SCAN_DB_LIMIT (64u*1024u*1024u)
#define SCAN_API_LIMIT (2u*1024u*1024u)
enum { SOURCE_DB, SOURCE_APP, SOURCE_META, SOURCE_SHADOW };
static const char *const source_names[]={"appdb","app","appmeta","shadowmount"};
typedef struct {
    char id[10]; unsigned sources;
    bool custom_icon;
    char param[512],icon[512],name[256],content[80],version[32];
} Ps5Title;
typedef struct {
    Ps5Title titles[SCAN_MAX_TITLES]; size_t count;
    unsigned counts[4],errors,entries,skipped_count;
    bool truncated,timed_out,skipped_truncated;
    char appdb[64];
    struct { char id[64],reason[96]; } skipped[32];
    uint64_t started,finished;
    sqlite3 *json;
    sqlite3_stmt *param_id;
} Ps5Scan;
static Ps5Scan *library_cache;
static uint64_t scan_ms(void) {
#ifdef SSPI_CONSOLE_HOST_TEST
    return GetTickCount64();
#else
    struct timespec ts; if (clock_gettime(CLOCK_MONOTONIC,&ts)) return 0;
    return (uint64_t)ts.tv_sec*1000+(uint64_t)ts.tv_nsec/1000000;
#endif
}
static bool scan_expired(Ps5Scan *s) {
    unsigned budget=3000;
#ifdef SSPI_CONSOLE_HOST_TEST
    budget=ps5_test_budget_ms;
#endif
    if (scan_ms()-s->started>=budget) s->timed_out=true;
    return s->timed_out;
}
static int scan_progress(void *s) { return scan_expired(s); }
static void scan_skip(Ps5Scan *s,const char *id,const char *reason) {
    if (s->skipped_count==32) { s->skipped_truncated=true; return; }
    snprintf(s->skipped[s->skipped_count].id,64,"%s",id);
    snprintf(s->skipped[s->skipped_count++].reason,96,"%s",reason);
}
static Ps5Title *scan_add(Ps5Scan *s,const char *id,unsigned source) {
    if (!ps5_library_id(id)) return NULL;
    size_t i=0; while (i<s->count && strcmp(s->titles[i].id,id)) i++;
    if (i==s->count) {
        if (i==SCAN_MAX_TITLES) { s->truncated=true; return NULL; }
        memcpy(s->titles[s->count++].id,id,10);
    }
    Ps5Title *t=&s->titles[i];
    if (!(t->sources&(1u<<source))) { t->sources|=1u<<source; s->counts[source]++; }
    return t;
}
static bool scan_step(Ps5Scan *s) {
    if (scan_expired(s)) return false;
    if (++s->entries>SCAN_MAX_ENTRIES) { s->truncated=true; return false; }
    return true;
}
/* Only local game/metadata trees; ct_kind rejects traversal and linked ancestors. */
static bool scan_path(const char *path) {
    return path && strlen(path)<480 && (!strncmp(path,"/user/",6)||!strncmp(path,"/mnt/",5)||
        !strncmp(path,"/data/",6)||!strncmp(path,"/system_ex/app/",15)||!strncmp(path,"/system_data/priv/appmeta/",26));
}
static void scan_location(Ps5Title *t,const char *location) {
    if (!t || !scan_path(location)) return;
    char dir[512],p[512]; snprintf(dir,sizeof(dir),"%s",location);
    size_t n=strlen(dir); while (n>1 && dir[n-1]=='/') dir[--n]=0;
    int kind=ct_kind(dir);
    if (kind==1) {
        char *last=strrchr(dir,'/'); if (!last) return;
        if (!strcmp(last+1,"icon0.png") && !t->icon[0]) snprintf(t->icon,sizeof(t->icon),"%s",dir);
        *last=0;
    } else if (kind!=2) return;
    const char *param=!memcmp(t->id,"PPSA",4)?"param.json":"param.sfo";
    for (unsigned sub=0;sub<2;sub++) {
        int rc=snprintf(p,sizeof(p),"%s/%s%s",dir,sub?"sce_sys/":"",param);
        if (rc>0 && (size_t)rc<sizeof(p) && !t->param[0] && ct_kind(p)==1) snprintf(t->param,sizeof(t->param),"%s",p);
        rc=snprintf(p,sizeof(p),"%s/%sicon0.png",dir,sub?"sce_sys/":"");
        if (rc>0 && (size_t)rc<sizeof(p) && !t->icon[0] && ct_kind(p)==1) snprintf(t->icon,sizeof(t->icon),"%s",p);
    }
}
static bool scan_param_id(Ps5Scan *s,const char *path,char id[10]) {
    if (!s->param_id) return false;
    char *json=malloc(CT_MAX_META+1); size_t size=0; bool found=false;
    if (!json) { s->errors++; return false; }
    if (!ct_read(path,(uint8_t *)json,CT_MAX_META,&size)) {
        json[size]=0; sqlite3_reset(s->param_id);
        sqlite3_bind_text(s->param_id,1,json,(int)size,SQLITE_STATIC);
        if (sqlite3_step(s->param_id)==SQLITE_ROW) {
            const char *value=(const char *)sqlite3_column_text(s->param_id,0);
            if (ps5_library_id(value)) { memcpy(id,value,10); found=true; }
        }
        sqlite3_reset(s->param_id); sqlite3_clear_bindings(s->param_id);
    }
    free(json); return found;
}
static void scan_folder(Ps5Scan *s,const char *root,const char *name,unsigned source) {
    if (!strcmp(name,".")||!strcmp(name,"..")) return;
    if (source!=SOURCE_SHADOW && !ps5_library_id(name)) return;
    char dir[512],path[512],id[10];
    int n=snprintf(dir,sizeof(dir),"%s/%s",root,name);
    if (n<0 || (size_t)n>=sizeof(dir)) { scan_skip(s,name,"path too long"); return; }
    int kind=ct_kind(dir);
#ifdef SSPI_CONSOLE_HOST_TEST
    if (ps5_test_unreadable && !strcmp(dir,ps5_test_unreadable)) kind=-1;
#endif
    if (kind!=2) { if (source!=SOURCE_SHADOW || kind<0) scan_skip(s,name,"folder inaccessible or linked"); return; }
    if (source==SOURCE_APP) {
        if (!ps5_app_verified(root,name)) { scan_skip(s,name,"app folder has no verified package or executable metadata"); return; }
        memcpy(id,name,10);
    } else if (source==SOURCE_META) {
        snprintf(path,sizeof(path),"%s/%s",dir,!memcmp(name,"PPSA",4)?"param.json":"param.sfo");
        if (ct_kind(path)!=1) { scan_skip(s,name,"appmeta param file missing or inaccessible"); return; }
        memcpy(id,name,10);
    } else {
        snprintf(path,sizeof(path),"%s/eboot.bin",dir);
        if (ct_kind(path)!=1) return;
        snprintf(path,sizeof(path),"%s/sce_sys/param.json",dir);
        if (!scan_param_id(s,path,id)) { scan_skip(s,name,"dump param.json missing, invalid or oversized"); return; }
    }
    scan_location(scan_add(s,id,source),dir);
}
static void scan_root(Ps5Scan *s,const char *root,unsigned source) {
    if (scan_expired(s) || s->entries>SCAN_MAX_ENTRIES) return;
    int k=ct_kind(root); if (!k) return;
    if (k!=2) { s->errors++; scan_skip(s,source_names[source],"scan directory inaccessible or linked"); return; }
#ifdef SSPI_CONSOLE_HOST_TEST
    char pattern[512]; snprintf(pattern,sizeof(pattern),".%s/*",root); WIN32_FIND_DATAA entry;
    HANDLE h=FindFirstFileA(pattern,&entry);
    if (h==INVALID_HANDLE_VALUE) { if (GetLastError()!=ERROR_FILE_NOT_FOUND) s->errors++; return; }
    do { if (!scan_step(s)) break; scan_folder(s,root,entry.cFileName,source); } while (FindNextFileA(h,&entry));
    FindClose(h);
#else
    DIR *dir=opendir(root); if (!dir) { s->errors++; scan_skip(s,source_names[source],"scan directory unreadable"); return; }
    while (scan_step(s)) { errno=0; struct dirent *entry=readdir(dir); if (!entry) { if (errno) s->errors++; break; } scan_folder(s,root,entry->d_name,source); }
    if (closedir(dir)) s->errors++;
#endif
}
static int scan_mkdir(const char *path) {
    int k=ct_kind(path); if (k==2) return 0; if (k) return -1;
    char p[512]; snprintf(p,sizeof(p),NATIVE_PREFIX "%s",path);
#ifdef SSPI_CONSOLE_HOST_TEST
    return _mkdir(p);
#else
    return mkdir(p,0700);
#endif
}
static int scan_copy(Ps5Scan *s,const char *from,const char *to) {
    if (ct_kind(from)!=1) return -1;
    char native[512]; snprintf(native,sizeof(native),NATIVE_PREFIX "%s",from);
    int in=scan_open(native,SCAN_READ_FLAGS); if (in<0) return -1;
    int out=scan_open(to,SCAN_WRITE_FLAGS,0600); if (out<0) { scan_close(in); return -1; }
    uint8_t bytes[65536]; size_t total=0; int rc=0;
    for (;;) {
        if (scan_expired(s)) { rc=-1; break; }
        int n=(int)scan_read(in,bytes,sizeof(bytes));
        if (n<0 && errno==EINTR) continue;
        if (n<0) { rc=-1; break; } if (!n) break;
        total+=(size_t)n; if (total>SCAN_DB_LIMIT) { rc=-1; break; }
        int at=0; while (at<n) { int w=(int)scan_write(out,bytes+at,(unsigned)(n-at)); if (w<0 && errno==EINTR) continue; if (w<=0) { rc=-1; break; } at+=w; }
        if (rc) break;
    }
    if (scan_close(in)) rc=-1; if (scan_close(out)) rc=-1; return rc;
}
static bool scan_same_file(const struct scan_stat *a,const struct scan_stat *b) {
    return a->st_size==b->st_size && a->st_mtime==b->st_mtime && a->st_ino==b->st_ino
#ifndef SSPI_CONSOLE_HOST_TEST
        && a->st_mtim.tv_nsec==b->st_mtim.tv_nsec && a->st_ctim.tv_nsec==b->st_ctim.tv_nsec
#endif
        ;
}
static void scan_db(Ps5Scan *s) {
    static const char *const suffix[]={"","-wal","-shm"};
    char temp[512]="",dest[512],source[512]; struct scan_stat before[3],after;
    bool present[3]={0}; sqlite3 *db=NULL; sqlite3_stmt *tables=NULL;
    const char *reason="missing"; bool recognized=false;
    if (ct_kind("/system_data/priv/mms/app.db")!=1) goto done;
    reason="snapshot failed";
    if (scan_mkdir("/data") || scan_mkdir(PS5_DATA_ROOT) || scan_mkdir(PS5_DATA_ROOT "/tmp")) goto done;
#ifdef SSPI_CONSOLE_HOST_TEST
    snprintf(temp,sizeof(temp),"./data/SSPI/tmp/library-%lu-%llu",(unsigned long)GetCurrentProcessId(),(unsigned long long)scan_ms());
    if (_mkdir(temp)) { temp[0]=0; goto done; }
#else
    snprintf(temp,sizeof(temp),PS5_DATA_ROOT "/tmp/library-XXXXXX");
    if (!mkdtemp(temp)) { temp[0]=0; goto done; }
#endif
    /* No SQLite handle ever touches the live DB, WAL or SHM. A copy counts only if all
       three files are unchanged across it; ShellUI writes are brief, so retry a little. */
    for (unsigned attempt=0;;attempt++) {
        for (unsigned i=0;i<3;i++) {
            present[i]=false;
            snprintf(source,sizeof(source),"/system_data/priv/mms/app.db%s",suffix[i]);
            int kind=ct_kind(source); if (!kind && i) continue; if (kind!=1) goto done;
            char native[512]; snprintf(native,sizeof(native),NATIVE_PREFIX "%s",source);
            if (scan_stat(native,&before[i]) || before[i].st_size<0 || (uint64_t)before[i].st_size>SCAN_DB_LIMIT) { reason="snapshot size limit"; goto done; }
            present[i]=true;
        }
        for (unsigned i=0;i<3;i++) if (present[i]) {
            snprintf(source,sizeof(source),"/system_data/priv/mms/app.db%s",suffix[i]);
            snprintf(dest,sizeof(dest),"%s/app.db%s",temp,suffix[i]);
            if (scan_copy(s,source,dest)) goto done;
        }
        bool stable=true;
        for (unsigned i=0;i<3;i++) {
            snprintf(source,sizeof(source),NATIVE_PREFIX "/system_data/priv/mms/app.db%s",suffix[i]);
            int rc=scan_stat(source,&after);
            if ((present[i] && (rc || !scan_same_file(&before[i],&after))) || (!present[i] && !rc)) stable=false;
        }
        if (stable) break;
        for (unsigned i=0;i<3;i++) { snprintf(dest,sizeof(dest),"%s/app.db%s",temp,suffix[i]); scan_unlink(dest); }
        if (attempt==2 || scan_expired(s)) { reason="changed during snapshot"; goto done; }
#ifdef SSPI_CONSOLE_HOST_TEST
        Sleep(100);
#else
        usleep(100000);
#endif
    }
    snprintf(dest,sizeof(dest),"%s/app.db",temp);
    reason="open failed";
    if (sqlite3_open_v2(dest,&db,SQLITE_OPEN_READONLY|SQLITE_OPEN_NOMUTEX,NULL)!=SQLITE_OK) goto done;
    sqlite3_limit(db,SQLITE_LIMIT_LENGTH,1024*1024); sqlite3_limit(db,SQLITE_LIMIT_COLUMN,256);
    sqlite3_progress_handler(db,1000,scan_progress,s); sqlite3_busy_timeout(db,0);
    sqlite3_exec(db,"PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA cache_size=-512; PRAGMA mmap_size=0;",NULL,NULL,NULL);
    reason="schema unavailable";
    if (sqlite3_prepare_v2(db,"SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' "
        "ORDER BY CASE WHEN name='tbl_contentinfo' THEN 0 WHEN name GLOB 'tbl_appbrowse*' THEN 1 WHEN name='tbl_appinfo' THEN 2 ELSE 3 END,name",-1,&tables,NULL)!=SQLITE_OK) goto done;
    unsigned table_count=0; int table_rc=SQLITE_DONE;
    while (!scan_expired(s) && (table_rc=sqlite3_step(tables))==SQLITE_ROW) {
        if (++table_count>128) { s->truncated=true; break; }
        const char *table=(const char *)sqlite3_column_text(tables,0);
        if (!table || strlen(table)>128) continue;
        char sql[400]; sqlite3_snprintf(sizeof(sql),sql,"SELECT * FROM \"%w\"",table);
        sqlite3_stmt *rows=NULL;
        if (sqlite3_prepare_v2(db,sql,-1,&rows,NULL)!=SQLITE_OK) { reason="table unreadable"; goto done; }
        int columns=sqlite3_column_count(rows),idcol=-1;
        for (int c=0;c<columns;c++) { const char *name=sqlite3_column_name(rows,c); if (!sqlite3_stricmp(name,"titleId")||!sqlite3_stricmp(name,"title_id")) idcol=c; }
        if (idcol<0) { sqlite3_finalize(rows); continue; }
        recognized=true; int row_rc=SQLITE_DONE;
        while (scan_step(s) && (row_rc=sqlite3_step(rows))==SQLITE_ROW) {
            const char *id=(const char *)sqlite3_column_text(rows,idcol);
            Ps5Title *t=scan_add(s,id,SOURCE_DB); if (!t) continue;
            for (int c=0;c<columns && !scan_expired(s);c++) {
                if (sqlite3_column_type(rows,c)!=SQLITE_TEXT || sqlite3_column_bytes(rows,c)>4096) continue;
                const char *value=(const char *)sqlite3_column_text(rows,c),*name=sqlite3_column_name(rows,c);
                if (!value) continue;
                if (!sqlite3_stricmp(name,"titleName") && !t->name[0]) snprintf(t->name,sizeof(t->name),"%s",value);
                if (!sqlite3_stricmp(name,"contentId") && !t->content[0]) snprintf(t->content,sizeof(t->content),"%s",value);
                if (!sqlite3_stricmp(name,"contentVersion") && !t->version[0]) snprintf(t->version,sizeof(t->version),"%s",value);
                if ((!sqlite3_stricmp(name,"icon0Info") || !sqlite3_stricmp(name,"iconPath")) && scan_path(value) && ct_kind(value)==1)
                    snprintf(t->icon,sizeof(t->icon),"%s",value);
                if ((!t->param[0] || !t->icon[0]) && (strstr(name,"Path")||strstr(name,"path")||!sqlite3_stricmp(name,"icon0Info")||!sqlite3_stricmp(name,"val")))
                    scan_location(t,value);
            }
        }
        sqlite3_finalize(rows);
        if (row_rc!=SQLITE_DONE && !s->timed_out && s->entries<=SCAN_MAX_ENTRIES) { reason="query failed"; goto done; }
        if (s->entries>SCAN_MAX_ENTRIES) break;
    }
    if (table_rc!=SQLITE_DONE && !s->timed_out && !s->truncated) { reason="schema query failed"; goto done; }
    reason=recognized ? NULL : "unknown schema";
done:
    sqlite3_finalize(tables); if (db) sqlite3_close(db);
    if (temp[0]) {
        for (unsigned i=0;i<3;i++) { snprintf(dest,sizeof(dest),"%s/app.db%s",temp,suffix[i]); scan_unlink(dest); }
        scan_rmdir(temp);
    }
    if (s->timed_out) reason="scan time budget";
    snprintf(s->appdb,sizeof(s->appdb),reason?"unavailable:%s":"ok",reason);
    if (reason) s->errors++;
}
static void scan_manual(Ps5Scan *s) {
    if (scan_expired(s)) return;
    const char *file="/data/shadowmount/manual.lst"; if (!ct_kind(file)) return;
    char *text=malloc(65537); size_t size=0;
    if (!text || ct_read(file,(uint8_t *)text,65536,&size)) { free(text); s->errors++; scan_skip(s,"shadowmount","manual list unreadable or oversized"); return; }
    text[size]=0;
    for (char *line=text;*line && scan_step(s);) {
        char *end=strchr(line,'\n'); if (end) *end=0;
        while (*line==' '||*line=='\t') line++;
        size_t n=strlen(line); while (n && (line[n-1]=='\r'||line[n-1]==' '||line[n-1]=='\t'||line[n-1]=='/')) line[--n]=0;
        if (*line && *line!='#' && scan_path(line)) {
            char *slash=strrchr(line,'/'); if (slash && slash!=line) { *slash=0; scan_folder(s,line,slash+1,SOURCE_SHADOW); }
        }
        if (!end) break; line=end+1;
    }
    free(text);
}
#ifndef SSPI_CONSOLE_HOST_TEST
static int scan_poll(int fd,short events,uint64_t until,Ps5Scan *s) {
    uint64_t now=scan_ms(); if (now>=until || scan_expired(s)) return -1;
    struct pollfd p={.fd=fd,.events=events};
    return poll(&p,1,(int)(until-now))>0 && (p.revents&(events|POLLHUP)) ? 0 : -1;
}
#endif
static void scan_shadow_api(Ps5Scan *s) {
    if (scan_expired(s) || !s->json) return;
    char *data=malloc(SCAN_API_LIMIT+1); if (!data) return; size_t size=0;
#ifdef SSPI_CONSOLE_HOST_TEST
    /* An isolated response fixture; host tests never connect to a device. */
    if (ct_read("/shadow-api.json",(uint8_t *)data,SCAN_API_LIMIT,&size)) { free(data); return; }
    data[size]=0; char *body=data;
#else
    int fd=socket(AF_INET,SOCK_STREAM,0); if (fd<0) { free(data); return; }
    if (fcntl(fd,F_SETFL,O_NONBLOCK)<0) goto api_done;
    struct sockaddr_in addr={.sin_len=sizeof(addr),.sin_family=AF_INET,.sin_port=htons(10101)};
    addr.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
    uint64_t until=scan_ms()+250; int rc=connect(fd,(struct sockaddr *)&addr,sizeof(addr));
    if (rc && (errno!=EINPROGRESS || scan_poll(fd,POLLOUT,until,s))) goto api_done;
    int error=0; socklen_t error_size=sizeof(error);
    if (getsockopt(fd,SOL_SOCKET,SO_ERROR,&error,&error_size) || error) goto api_done;
    const char request[]="POST /api/v1/games HTTP/1.0\r\nHost: 127.0.0.1:10101\r\nContent-Type: application/json\r\nContent-Length: 22\r\nConnection: close\r\n\r\n{\"include_size\":false}";
    /* The entire optional exchange has a 500 ms deadline, not a per-read timeout. */
    until=scan_ms()+500; size_t sent=0;
    while (sent<sizeof(request)-1) {
        if (scan_poll(fd,POLLOUT,until,s)) goto api_done;
        int n=(int)send(fd,request+sent,sizeof(request)-1-sent,0);
        if (n<0 && (errno==EINTR || errno==EAGAIN)) continue; if (n<=0) goto api_done; sent+=(size_t)n;
    }
    while (size<SCAN_API_LIMIT) {
        if (scan_poll(fd,POLLIN,until,s)) goto api_done;
        int n=(int)recv(fd,data+size,SCAN_API_LIMIT-size,0);
        if (n<0 && (errno==EINTR || errno==EAGAIN)) continue; if (n<0) goto api_done; if (!n) break; size+=(size_t)n;
    }
    if (size==SCAN_API_LIMIT) goto api_done;
    data[size]=0;
    if (strncmp(data,"HTTP/1.0 200 ",13) && strncmp(data,"HTTP/1.1 200 ",13)) goto api_done;
    char *body=strstr(data,"\r\n\r\n"); if (!body) goto api_done; body+=4;
#endif
    sqlite3_stmt *rows=NULL;
    if (sqlite3_prepare_v2(s->json,"SELECT json_extract(value,'$.title_id'),json_extract(value,'$.runtime_path'),"
        "json_extract(value,'$.path'),json_extract(value,'$.managed'),json_extract(value,'$.installed') "
        "FROM json_each(?1,'$.games') WHERE json_extract(?1,'$.status')=0",-1,&rows,NULL)==SQLITE_OK) {
        sqlite3_bind_text(rows,1,body,-1,SQLITE_STATIC);
        while (scan_step(s) && sqlite3_step(rows)==SQLITE_ROW) {
            if (!sqlite3_column_int(rows,3) || !sqlite3_column_int(rows,4)) continue;
            const char *id=(const char *)sqlite3_column_text(rows,0);
            Ps5Title *t=scan_add(s,id,SOURCE_SHADOW);
            scan_location(t,(const char *)sqlite3_column_text(rows,1));
            scan_location(t,(const char *)sqlite3_column_text(rows,2));
        }
    }
    sqlite3_finalize(rows);
#ifndef SSPI_CONSOLE_HOST_TEST
api_done:
    close(fd);
#endif
    free(data);
}
static int scan_collect(bool force) {
    if (!library_cache) library_cache=calloc(1,sizeof(*library_cache));
    if (!library_cache) return -1;
    Ps5Scan *s=library_cache;
    if (!force && s->finished && scan_ms()-s->finished<30000) return 0;
    memset(s,0,sizeof(*s)); s->started=scan_ms();
    sqlite3_hard_heap_limit64(16*1024*1024);
    if (sqlite3_open(":memory:",&s->json)==SQLITE_OK) {
        sqlite3_limit(s->json,SQLITE_LIMIT_LENGTH,SCAN_API_LIMIT);
        sqlite3_progress_handler(s->json,1000,scan_progress,s);
        if (sqlite3_prepare_v2(s->json,"SELECT coalesce(json_extract(?1,'$.titleId'),json_extract(?1,'$.title_id'))",-1,&s->param_id,NULL)!=SQLITE_OK) s->errors++;
    } else s->errors++;
    scan_db(s);
    for (size_t i=0;i<sizeof(app_roots)/sizeof(app_roots[0]);i++) scan_root(s,app_roots[i],SOURCE_APP);
    for (size_t i=0;i<ct_metadata_root_count;i++) scan_root(s,ct_metadata_roots[i],SOURCE_META);
    const char *roots[]={"/data/homebrew","/data/etaHEN/games","/mnt/ext0/homebrew","/mnt/ext0/etaHEN/games","/mnt/ext1/homebrew","/mnt/ext1/etaHEN/games"};
    for (size_t i=0;i<sizeof(roots)/sizeof(roots[0]);i++) scan_root(s,roots[i],SOURCE_SHADOW);
    for (unsigned i=0;i<8;i++) {
        char path[64]; snprintf(path,sizeof(path),"/mnt/usb%u/homebrew",i); scan_root(s,path,SOURCE_SHADOW);
        snprintf(path,sizeof(path),"/mnt/usb%u/etaHEN/games",i); scan_root(s,path,SOURCE_SHADOW);
    }
    scan_manual(s); scan_shadow_api(s);
    for (size_t i=0;i<s->count && !scan_expired(s);i++) s->titles[i].custom_icon=ct_custom_icon(PS5_DATA_ROOT,s->titles[i].id);
    sqlite3_finalize(s->param_id); s->param_id=NULL; if (s->json) sqlite3_close(s->json); s->json=NULL;
    if (scan_expired(s)) scan_skip(s,"scan","3 second scan budget exceeded");
    s->finished=scan_ms(); return 0;
}
static bool ps5_lookup(const char *id,Ps5Title *out) {
    if (!ps5_library_id(id)) return false;
    bool found=false; SCAN_LOCK();
    if (!scan_collect(false)) for (size_t i=0;i<library_cache->count;i++) if (!strcmp(library_cache->titles[i].id,id)) {
        if (out) *out=library_cache->titles[i]; found=true; break;
    }
    SCAN_UNLOCK(); return found;
}
bool ps5_title_installed(const char *id) { return ps5_lookup(id,NULL); }
int ps5_icon_path(const char *id,char *path,size_t cap) {
    Ps5Title title; if (!ps5_lookup(id,&title) || !title.icon[0]) return -1;
    /* Appmeta is already handled by console_files; do not write a copy twice. */
    for (size_t i=0;i<ct_metadata_root_count;i++) {
        char existing[512]; snprintf(existing,sizeof(existing),"%s/%s/icon0.png",ct_metadata_roots[i],id);
        if (!strcmp(existing,title.icon)) return -1;
    }
    int n=snprintf(path,cap,"%s",title.icon); return n<0 || (size_t)n>=cap ? -1 : 0;
}
int ps5_library_json(char *out,size_t cap) {
    SCAN_LOCK();
    if (scan_collect(true)) { SCAN_UNLOCK(); return -1; }
    Ps5Scan *s=library_cache; size_t at=0;
    /* 82 bytes/title covers all four sources, title and custom-icon lists. */
    size_t count=s->count, reserve=16384;
    if (cap<reserve) { SCAN_UNLOCK(); return -1; }
    if (count>(cap-reserve)/82) { count=(cap-reserve)/82; s->truncated=true; }
#define ADD(...) do { int added=snprintf(out+at,cap-at,__VA_ARGS__); if(added<0 || (size_t)added>=cap-at) { SCAN_UNLOCK(); return -1; } at+=(size_t)added; } while(0)
    ADD("{\"titles\":[");
    for (size_t i=0;i<count;i++) ADD("%s\"%s\"",i?",":"",s->titles[i].id);
    ADD("],\"complete\":%s,\"truncated\":%s,\"errorsTruncated\":false,\"errors\":[%s],\"customIcons\":[",
        !s->errors&&!s->truncated&&!s->timed_out?"true":"false",s->truncated?"true":"false",
        s->errors?"\"Some library sources could not be read; see diagnostics.\"":s->timed_out?"\"The library scan time budget was exceeded.\"":"");
    unsigned custom=0;
    for (size_t i=0;i<count;i++) if (s->titles[i].custom_icon) ADD("%s\"%s\"",custom++?",":"",s->titles[i].id);
    ADD("],\"sources\":{");
    for (size_t i=0;i<count;i++) {
        ADD("%s\"%s\":[",i?",":"",s->titles[i].id); unsigned n=0;
        for (unsigned j=0;j<4;j++) if (s->titles[i].sources&(1u<<j)) ADD("%s\"%s\"",n++?",":"",source_names[j]);
        ADD("]");
    }
    ADD("},\"diagnostics\":{\"appdb\":\"%s\",\"counts\":{\"appdb\":%u,\"app\":%u,\"appmeta\":%u,\"shadowmount\":%u},"
        "\"elapsedMs\":%llu,\"budgetExceeded\":%s,\"skippedTruncated\":%s,\"skipped\":[",s->appdb,s->counts[0],s->counts[1],s->counts[2],s->counts[3],
        (unsigned long long)(s->finished-s->started),s->timed_out?"true":"false",s->skipped_truncated?"true":"false");
    for (unsigned i=0;i<s->skipped_count;i++) {
        char id[160],reason[220]; ct_json_quote(id,sizeof(id),s->skipped[i].id); ct_json_quote(reason,sizeof(reason),s->skipped[i].reason);
        ADD("%s{\"id\":%s,\"reason\":%s}",i?",":"",id,reason);
    }
    ADD("]}}");
#undef ADD
    SCAN_UNLOCK(); return 0;
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
    if (!patch) {
        Ps5Title title;
        if (ps5_lookup(id,&title)) {
            if (title.param[0] && !ct_read(title.param,out,CT_MAX_META,size) && *size) return 0;
            if (title.name[0]) {
                if (ppsa) {
                    char name[520],content[170],version[70]; ct_json_quote(name,sizeof(name),title.name);
                    ct_json_quote(content,sizeof(content),title.content); ct_json_quote(version,sizeof(version),title.version);
                    int n=snprintf((char *)out,CT_MAX_META,"{\"titleId\":\"%s\",\"titleName\":%s,\"contentId\":%s,\"contentVersion\":%s}",id,name,content,version);
                    if (n>0 && n<(int)CT_MAX_META) { *size=(size_t)n; return 0; }
                } else {
                    /* Minimal SFO using only values actually present in app.db. */
                    const char *keys[]={"TITLE_ID","TITLE","CONTENT_ID","APP_VER"};
                    const char *values[]={id,title.name,title.content,title.version};
                    size_t fields=2+(title.content[0]!=0)+(title.version[0]!=0),key_at=20+fields*16,value_at=key_at;
                    for (unsigned i=0;i<4;i++) if (values[i][0]) value_at+=strlen(keys[i])+1;
                    value_at=(value_at+3)&~(size_t)3;
                    memset(out,0,1024); memcpy(out,"\0PSF",4); meta_le32(out+4,0x101);
                    meta_le32(out+8,(uint32_t)key_at); meta_le32(out+12,(uint32_t)value_at); meta_le32(out+16,(uint32_t)fields);
                    size_t k=0,v=0,field=0;
                    for (unsigned i=0;i<4;i++) if (values[i][0]) {
                        uint8_t *e=out+20+field++*16; size_t n=strlen(values[i])+1,aligned=(n+3)&~(size_t)3;
                        e[0]=(uint8_t)k; e[1]=(uint8_t)(k>>8); e[2]=4; e[3]=2;
                        meta_le32(e+4,(uint32_t)n); meta_le32(e+8,(uint32_t)aligned); meta_le32(e+12,(uint32_t)v);
                        memcpy(out+key_at+k,keys[i],strlen(keys[i])+1); memcpy(out+value_at+v,values[i],n);
                        k+=strlen(keys[i])+1; v+=aligned;
                    }
                    *size=value_at+v; return 0;
                }
            }
        }
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
