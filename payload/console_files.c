#include "console_files.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#ifdef SSPI_CONSOLE_HOST_TEST
#include <windows.h>
#include <direct.h>
#include <io.h>
#define CT_CLOSE _close
#define CT_WRITE _write
#define CT_READ _read
#define CT_SYNC _commit
#define CT_UNLINK _unlink
int ct_test_fail_write, ct_test_fail_rename;
int ct_test_fail_rename_after=-1;
#else
#include <unistd.h>
#define CT_CLOSE close
#define CT_WRITE write
#define CT_READ read
#define CT_SYNC fsync
#define CT_UNLINK unlink
#endif

const char *const ct_metadata_roots[] = {
    "/user/appmeta", "/user/appmeta/external", "/system_data/priv/appmeta",
    "/system_data/priv/appmeta/external", "/mnt/ext0/user/appmeta",
#ifndef CT_PS4
    "/mnt/ext1/user/appmeta", "/mnt/ext0/user/appmeta/external", "/mnt/ext1/user/appmeta/external",
#endif
};
const size_t ct_metadata_root_count = sizeof(ct_metadata_roots) / sizeof(ct_metadata_roots[0]);
#ifdef CT_PS4
#define CT_ICON_SLOTS ct_metadata_root_count
#else
extern int ps5_icon_path(const char *id,char *path,size_t cap);
#define CT_ICON_SLOTS (ct_metadata_root_count+1)
#endif

bool ct_valid_id(const char *id) {
    if (!id || strlen(id) != 9) return false;
    for (unsigned i=0; i<9; ++i) if (i<4 ? (id[i]<'A'||id[i]>'Z') : (id[i]<'0'||id[i]>'9')) return false;
    return true;
}
bool ct_ps4_title_id(const char *id) {
    if (!ct_valid_id(id)) return false;
    static const char *const prefixes[]={"CUSA","SLUS","SLES","SCUS","SCES","SLPS","SLPM","SCPS","SCAJ","SLAJ","SLKA","SLKS","SCKA"};
    for (size_t i=0;i<sizeof(prefixes)/sizeof(prefixes[0]);i++) if (!memcmp(id,prefixes[i],4)) return true;
    return false;
}
static uint32_t ct_be32(const uint8_t *p) { return ((uint32_t)p[0]<<24)|((uint32_t)p[1]<<16)|((uint32_t)p[2]<<8)|p[3]; }
static uint32_t ct_crc(const uint8_t *p, size_t n) {
    uint32_t crc=0xffffffffu;
    while (n--) { crc^=*p++; for (unsigned bit=0;bit<8;bit++) crc=(crc>>1)^((0u-(crc&1u))&0xedb88320u); }
    return ~crc;
}
bool ct_valid_png(const uint8_t *p, size_t n) {
    if (!p || n<57 || n>CT_MAX_PNG || memcmp(p,"\x89PNG\r\n\x1a\n",8) ||
        ct_be32(p+8)!=13 || memcmp(p+12,"IHDR",4)) return false;
    uint32_t w=ct_be32(p+16), h=ct_be32(p+20);
    if (w<256 || w>1024 || h!=w || p[26] || p[27] || p[28]>1) return false;
    unsigned depth=p[24], color=p[25];
    if (!((color==0 && (depth==1||depth==2||depth==4||depth==8||depth==16)) ||
        (color==3 && (depth==1||depth==2||depth==4||depth==8)) ||
        ((color==2||color==4||color==6) && (depth==8||depth==16)))) return false;
    bool data=false; size_t at=8;
    while (at<n) {
        if (n-at<12) return false;
        size_t length=ct_be32(p+at);
        if (length>n-at-12 || ct_crc(p+at+4,length+4)!=ct_be32(p+at+8+length)) return false;
        if (at!=8 && !memcmp(p+at+4,"IHDR",4)) return false;
        if (!memcmp(p+at+4,"IDAT",4) && length) data=true;
        if (!memcmp(p+at+4,"IEND",4)) return !length && data && at+12==n;
        at+=length+12;
    }
    return false;
}

static int ct_path(const char *path, char *out, size_t capacity) {
    if (!path || path[0]!='/' || strchr(path,'\\') || strchr(path,':') || strstr(path,"//")) return -1;
    for (const char *p=path; *p; ++p) if ((unsigned char)*p<32 || (*p=='/' && p[1]=='.' &&
        (!p[2] || p[2]=='/' || (p[2]=='.' && (!p[3] || p[3]=='/'))))) return -1;
    int n=snprintf(out,capacity,
#ifdef SSPI_CONSOLE_HOST_TEST
        ".%s",
#else
        "%s",
#endif
        path);
    return n<0 || (size_t)n>=capacity ? -1 : 0;
}
static int ct_native_kind(const char *p) {
#ifdef SSPI_CONSOLE_HOST_TEST
    DWORD a=GetFileAttributesA(p);
    if (a==INVALID_FILE_ATTRIBUTES) return GetLastError()==ERROR_FILE_NOT_FOUND || GetLastError()==ERROR_PATH_NOT_FOUND ? 0 : -1;
    if (a&FILE_ATTRIBUTE_REPARSE_POINT) return -1;
    return a&FILE_ATTRIBUTE_DIRECTORY ? 2 : 1;
#else
    struct stat st;
    if (lstat(p,&st)) return errno==ENOENT ? 0 : -1;
    return S_ISREG(st.st_mode) ? 1 : S_ISDIR(st.st_mode) ? 2 : -1;
#endif
}
int ct_kind(const char *path) {
    char p[512]; if (ct_path(path,p,sizeof(p))) return -1;
    for (char *s=p+1; *s; ++s) if (*s=='/') {
        *s=0; int k=ct_native_kind(p); *s='/';
        if (!k) return 0;
        if (k!=2) return -1;
    }
    return ct_native_kind(p);
}
static int ct_open(const char *path, bool create) {
    char p[512]; if (ct_path(path,p,sizeof(p))) return -1;
    int k=ct_kind(path); if (create ? k!=0 : k!=1) return -1;
#ifdef SSPI_CONSOLE_HOST_TEST
    return _open(p,_O_BINARY|(create ? _O_WRONLY|_O_CREAT|_O_EXCL : _O_RDONLY),_S_IREAD|_S_IWRITE);
#else
    return open(p,O_NOFOLLOW|(create ? O_WRONLY|O_CREAT|O_EXCL : O_RDONLY),0644);
#endif
}
int ct_read(const char *path, uint8_t *out, size_t cap, size_t *size) {
    int fd=ct_open(path,false); if (fd<0) return -1;
    size_t n=0; int rc=0;
    while (n<cap) { int got=(int)CT_READ(fd,out+n,(unsigned)(cap-n)); if (got<0 && errno==EINTR) continue;
        if (got<0) { rc=-1; break; } if (!got) break; n+=(size_t)got; }
    uint8_t extra; if (!rc && n==cap && CT_READ(fd,&extra,1)!=0) rc=-1;
    if (CT_CLOSE(fd)) rc=-1;
    if (!rc) *size=n;
    return rc;
}
int ct_read_at(const char *path, uint8_t *out, size_t size, uint64_t offset) {
    if (offset>INT64_MAX || size>(uint64_t)INT64_MAX-offset) return -1;
    int fd=ct_open(path,false); if (fd<0) return -1;
    size_t at=0; int rc=0;
    while (at<size) {
#ifdef SSPI_CONSOLE_HOST_TEST
        int got=_lseeki64(fd,(__int64)(offset+at),SEEK_SET)<0 ? -1 : _read(fd,out+at,(unsigned)(size-at));
#else
        int got=(int)pread(fd,out+at,size-at,(off_t)(offset+at));
#endif
        if (got<0 && errno==EINTR) continue;
        if (got<=0) { rc=-1; break; } at+=(size_t)got;
    }
    if (CT_CLOSE(fd)) rc=-1;
    return rc;
}
static int ct_remove(const char *path) {
    char p[512]; if (ct_kind(path)!=1 || ct_path(path,p,sizeof(p))) return -1;
    return CT_UNLINK(p);
}
int ct_atomic_write(const char *path, const uint8_t *bytes, size_t size) {
    char temp[512], from[512], to[512];
    int k=ct_kind(path); if (k!=0 && k!=1) return -1;
    int n=snprintf(temp,sizeof(temp),"%s.sspi-next",path);
    if (n<0 || (size_t)n>=sizeof(temp)) return -1;
    /* O_EXCL refuses stale files and links, including hard links. Never truncate one. */
    int fd=ct_open(temp,true); if (fd<0) return -1;
    int rc=0; bool renamed=false; size_t at=0;
    while (at<size) {
#ifdef SSPI_CONSOLE_HOST_TEST
        if (ct_test_fail_write) { rc=-1; break; }
#endif
        int wrote=(int)CT_WRITE(fd,bytes+at,(unsigned)(size-at));
        if (wrote<0 && errno==EINTR) continue;
        if (wrote<=0) { rc=-1; break; } at+=(size_t)wrote;
    }
    if (CT_SYNC(fd)) rc=-1;
    if (CT_CLOSE(fd)) rc=-1;
    k=ct_kind(path); if (k!=0 && k!=1) rc=-1;
    if (ct_path(temp,from,sizeof(from)) || ct_path(path,to,sizeof(to))) rc=-1;
    if (!rc) {
#ifdef SSPI_CONSOLE_HOST_TEST
        rc=ct_test_fail_rename || ct_test_fail_rename_after==0 || !MoveFileExA(from,to,MOVEFILE_REPLACE_EXISTING|MOVEFILE_WRITE_THROUGH) ? -1 : 0;
        if (ct_test_fail_rename_after>0) ct_test_fail_rename_after--;
#else
        rc=rename(from,to);
        if (!rc) {
            renamed=true;
            char *slash=strrchr(to,'/'); *slash=0;
            int parent=open(to,O_RDONLY|O_DIRECTORY|O_NOFOLLOW);
            if (parent<0) rc=-1;
            else { if (fsync(parent) && errno!=EINVAL && errno!=ENOTSUP) rc=-1; if (close(parent)) rc=-1; }
        }
#endif
    }
    if (rc) (void)ct_remove(temp);
    return rc && renamed ? 1 : rc;
}
static int ct_backup_dir(const char *root) {
    if (ct_kind(root)!=2) return -1;
    char path[512], native[512]; snprintf(path,sizeof(path),"%s/icon-originals",root);
    if (ct_kind(path)==2) return 0;
    if (ct_kind(path)!=0 || ct_path(path,native,sizeof(native))) return -1;
#ifdef SSPI_CONSOLE_HOST_TEST
    int rc=_mkdir(native);
#else
    int rc=mkdir(native,0700);
#endif
    return rc || ct_kind(path)!=2 ? -1 : 0;
}
static int ct_icon_paths(const char *root, const char *id, size_t slot, char *target, char *backup) {
    snprintf(backup,512,"%s/icon-originals/%s-%u.png",root,id,(unsigned)slot);
    if (slot<ct_metadata_root_count) snprintf(target,512,"%s/%s/icon0.png",ct_metadata_roots[slot],id);
#ifndef CT_PS4
    else if (ps5_icon_path(id,target,512)) return -1;
#endif
    return 0;
}
bool ct_custom_icon(const char *root, const char *id) {
    if (!ct_valid_id(id)) return false;
    for (size_t i=0;i<CT_ICON_SLOTS;i++) { char backup[512]; snprintf(backup,sizeof(backup),"%s/icon-originals/%s-%u.png",root,id,(unsigned)i); if (ct_kind(backup)==1) return true; }
    return false;
}
int ct_icon_get(const char *root, const char *id, bool original, uint8_t *out, size_t *size) {
    if (!ct_valid_id(id)) return -1;
    if (original) for (size_t i=0;i<CT_ICON_SLOTS;i++) {
        char backup[512]; snprintf(backup,sizeof(backup),"%s/icon-originals/%s-%u.png",root,id,(unsigned)i);
        int k=ct_kind(backup); if (k<0) return -1;
        if (k) return ct_read(backup,out,CT_MAX_PNG,size) || !ct_valid_png(out,*size) ? -1 : 0;
    }
    for (size_t i=0;i<CT_ICON_SLOTS;i++) {
        char target[512],backup[512]; if (ct_icon_paths(root,id,i,target,backup)) continue;
        if (!ct_read(target,out,CT_MAX_PNG,size) && ct_valid_png(out,*size)) return 0;
    }
    return -1;
}
int ct_icon_change(const char *root, const char *platform, const char *id,
    const uint8_t *png, size_t size, bool restore, char *out, size_t cap) {
    const char *error="Installed icon metadata is unavailable.";
    if (!ct_valid_id(id) || (!restore && !ct_valid_png(png,size))) { error="Use a square PNG from 256 to 1024 pixels, at most 2 MiB, and a valid title ID."; goto fail; }
    if (ct_backup_dir(root)) { error="The receiver could not open its original-icon folder."; goto fail; }
    uint8_t *original=malloc(CT_MAX_PNG); if (!original) { error="Not enough memory to save the icon."; goto fail; }
    bool selected[9]={0}, backed=false, durable=true; unsigned count=0, written=0;
    char targets[9][512],backups[9][512];
    /* Preflight every existing copy and save originals before changing any icon. */
    for (size_t i=0;i<CT_ICON_SLOTS;i++) {
        char *target=targets[i],*backup=backups[i];
        if (ct_icon_paths(root,id,i,target,backup)) {
            if (restore && ct_kind(backup)==1) { error="An original icon still belongs to unavailable storage. Reconnect it and retry."; goto failed_buffer; }
            continue;
        }
        int tk=ct_kind(target), bk=ct_kind(backup); size_t length=0;
        if (tk<0 || bk<0 || tk==2 || bk==2) { error="An icon metadata path is linked or inaccessible."; goto failed_buffer; }
        if (restore ? !bk : !tk) continue;
        if (tk!=1) { error="An original icon still belongs to unavailable storage. Reconnect it and retry."; goto failed_buffer; }
        if (ct_read(bk ? backup : target,original,CT_MAX_PNG,&length) || !ct_valid_png(original,length)) { error="An original icon is invalid or unreadable; no originals were overwritten."; goto failed_buffer; }
        if (!restore && !bk) { if (ct_atomic_write(backup,original,length)) { error="Could not save the original icon."; goto failed_buffer; } backed=true; }
        selected[i]=true; count++;
    }
    if (!count) { error=restore ? "No saved original icon is available for this title." : "Installed icon metadata is unavailable."; goto failed_buffer; }
    for (size_t i=0;i<CT_ICON_SLOTS;i++) if (selected[i]) {
        const char *target=targets[i],*backup=backups[i]; size_t length=size; const uint8_t *bytes=png;
        if (restore) { if (ct_read(backup,original,CT_MAX_PNG,&length) || !ct_valid_png(original,length)) break; bytes=original; }
        if (ct_kind(target)!=1) break;
        int rc=ct_atomic_write(target,bytes,length);
        if (rc<0) break;
        written++;
        if (rc>0) { durable=false; break; }
    }
    bool cleanup=true;
    if (restore && written==count && durable) for (size_t i=0;i<CT_ICON_SLOTS;i++) if (selected[i]) {
        if (ct_remove(backups[i])) cleanup=false;
    }
    free(original);
    if (!written) { error="Could not replace the icon. Originals are preserved; retry the operation."; goto fail; }
    char message[256];
    if (written!=count || !durable) snprintf(message,sizeof(message),"%u of %u icon copies written. Originals are preserved; retry before restarting your %s.",written,count,platform);
    else if (!cleanup) snprintf(message,sizeof(message),"Original icons restored; some saved copies could not be removed. Retry before restarting your %s.",platform);
    else snprintf(message,sizeof(message),"%s Restart your %s to see the new icons.",restore ? "Original icons restored." : "Icon PNG copies saved.",platform);
    int n=snprintf(out,cap,"{\"titleId\":\"%s\",\"written\":%u,\"backedUp\":%s,\"refresh\":\"restart-required\",\"message\":\"%s\"}",id,written,backed?"true":"false",message);
    return n<0 || (size_t)n>=cap ? -1 : 0;
failed_buffer:
    free(original);
fail:
    snprintf(out,cap,"%s",error); return -1;
}

#ifdef CT_PS4
/* A PS4 title keeps its icon twice in each metadata folder: icon0.png and icon0.dds, which the
   home screen draws (DXT1, so a mask's transparent corners are its one-bit alpha). Some titles add
   per-language copies, icon0_00 to icon0_30. Every copy present is replaced; every original is
   saved before any copy changes, and restore puts back whatever was saved. */
#define CT_ICON_VARIANTS 32u
static uint32_t ct_le32(const uint8_t *p) { return (uint32_t)p[0]|((uint32_t)p[1]<<8)|((uint32_t)p[2]<<16)|((uint32_t)p[3]<<24); }
/* New icons must look like the ones the console ships: square DXT1, one level, 256 to 1024 px. */
bool ct_valid_dds(const uint8_t *p, size_t n) {
    if (!p || n<128 || n>CT_MAX_DDS || memcmp(p,"DDS ",4) || ct_le32(p+4)!=124) return false;
    uint32_t h=ct_le32(p+12), w=ct_le32(p+16), mips=ct_le32(p+28);
    if (w!=h || (w!=256 && w!=512 && w!=1024) || mips>1 || ct_le32(p+76)!=32 || !(ct_le32(p+80)&4u) || memcmp(p+84,"DXT1",4)) return false;
    return n==128u+(size_t)(w/4u)*(h/4u)*8u;
}
/* Originals only need to be DDS files; games may ship other block formats. */
static bool ct_dds_file(const uint8_t *p, size_t n) { return p && n>=128 && n<=CT_MAX_PNG && !memcmp(p,"DDS ",4) && ct_le32(p+4)==124; }
typedef struct { char target[160]; char backup[192]; bool dds; } CtIconCopy;
static void ct_copy_paths(CtIconCopy *c, const char *root, const char *id, size_t slot, unsigned variant, bool dds) {
    const char *ext=dds?"dds":"png";
    if (!variant) snprintf(c->target,sizeof(c->target),"%s/%s/icon0.%s",ct_metadata_roots[slot],id,ext);
    else snprintf(c->target,sizeof(c->target),"%s/%s/icon0_%02u.%s",ct_metadata_roots[slot],id,variant-1,ext);
    /* The base PNG keeps the name older receivers used, so their changes restore too. */
    if (!variant) snprintf(c->backup,sizeof(c->backup),"%s/icon-originals/%s-%u.%s",root,id,(unsigned)slot,ext);
    else snprintf(c->backup,sizeof(c->backup),"%s/icon-originals/%s-%u-%02u.%s",root,id,(unsigned)slot,variant-1,ext);
    c->dds=dds;
}
int ct_ps4_icon_change(const char *root, const char *id, const uint8_t *png, size_t png_size,
    const uint8_t *dds, size_t dds_size, bool restore, char *out, size_t cap) {
    const char *error="Installed icon metadata is unavailable.";
    CtIconCopy *copies=NULL; uint8_t *original=NULL;
    size_t count=0; unsigned written=0, home=0; bool backed=false, durable=true;
    if (!ct_valid_id(id) || (!restore && (!ct_valid_png(png,png_size) || (dds && !ct_valid_dds(dds,dds_size))))) {
        error="Use a square PNG from 256 to 1024 pixels (at most 2 MiB), a matching DXT1 DDS, and a valid title ID."; goto fail;
    }
    if (ct_backup_dir(root)) { error="The receiver could not open its original-icon folder."; goto fail; }
    copies=calloc(ct_metadata_root_count*CT_ICON_VARIANTS*2u,sizeof(*copies)); original=malloc(CT_MAX_PNG);
    if (!copies || !original) { error="Not enough memory to save the icon."; goto fail; }
    /* Preflight every copy and save originals before changing any icon. */
    for (size_t slot=0;slot<ct_metadata_root_count;slot++) {
        char folder[160]; snprintf(folder,sizeof(folder),"%s/%s",ct_metadata_roots[slot],id);
        int fk=ct_kind(folder);
        if (fk<0 || fk==1) { error="An icon metadata path is linked or inaccessible."; goto fail; }
        if (!fk && !restore) continue;
        for (unsigned v=0;v<CT_ICON_VARIANTS;v++) for (int kind=0;kind<2;kind++) {
            bool is_dds=kind==1;
            if (!restore && is_dds && !dds) continue;
            CtIconCopy *c=&copies[count]; ct_copy_paths(c,root,id,slot,v,is_dds);
            int tk=fk ? ct_kind(c->target) : 0, bk=ct_kind(c->backup); size_t length=0;
            if (tk<0 || bk<0 || tk==2 || bk==2) { error="An icon metadata path is linked or inaccessible."; goto fail; }
            if (restore ? !bk : !tk) continue;
            if (tk!=1) { error="An original icon still belongs to unavailable storage. Reconnect it and retry."; goto fail; }
            if (ct_read(bk ? c->backup : c->target,original,CT_MAX_PNG,&length) || !(is_dds ? ct_dds_file(original,length) : ct_valid_png(original,length))) {
                error="An original icon is invalid or unreadable; no originals were overwritten."; goto fail;
            }
            if (!restore && !bk) { if (ct_atomic_write(c->backup,original,length)) { error="Could not save the original icon."; goto fail; } backed=true; }
            count++;
        }
    }
    if (!count) { error=restore ? "No saved original icon is available for this title." : "Installed icon metadata is unavailable."; goto fail; }
    for (size_t i=0;i<count;i++) {
        const CtIconCopy *c=&copies[i]; size_t length=c->dds ? dds_size : png_size; const uint8_t *bytes=c->dds ? dds : png;
        if (restore) { if (ct_read(c->backup,original,CT_MAX_PNG,&length)) break; bytes=original; }
        if (ct_kind(c->target)!=1) break;
        int rc=ct_atomic_write(c->target,bytes,length);
        if (rc<0) break;
        written++; if (c->dds) home++;
        if (rc>0) { durable=false; break; }
    }
    bool cleanup=true;
    if (restore && written==count && durable) for (size_t i=0;i<count;i++) if (ct_remove(copies[i].backup)) cleanup=false;
    free(original); free(copies); original=NULL; copies=NULL;
    if (!written) { error="Could not replace the icon. Originals are preserved; retry the operation."; goto fail; }
    char message[256];
    if (written!=count || !durable) snprintf(message,sizeof(message),"%u of %u icon copies written. Originals are preserved; retry before restarting your PS4.",written,(unsigned)count);
    else if (!cleanup) snprintf(message,sizeof(message),"Original icons restored; some saved copies could not be removed. Retry before restarting your PS4.");
    else if (restore) snprintf(message,sizeof(message),"Original icons restored. Restart your PS4 to see them.");
    else snprintf(message,sizeof(message),"%u icon copies saved%s. Restart your PS4 to see the new icons.",written,home ? ", including the home screen's" : "");
    int n=snprintf(out,cap,"{\"titleId\":\"%s\",\"written\":%u,\"backedUp\":%s,\"refresh\":\"restart-required\",\"message\":\"%s\"}",id,written,backed?"true":"false",message);
    return n<0 || (size_t)n>=cap ? -1 : 0;
fail:
    free(original); free(copies);
    snprintf(out,cap,"%s",error); return -1;
}
#endif
void ct_json_quote(char *out, size_t cap, const char *text) {
    size_t n=0; if (cap<3) return; out[n++]='"';
    for (;*text && n+7<cap;text++) {
        unsigned char c=(unsigned char)*text;
        if (c=='"'||c=='\\') { out[n++]='\\'; out[n++]=(char)c; }
        else if (c<32) out[n++]=' ';
        else out[n++]=(char)c;
    }
    out[n++]='"'; out[n]=0;
}
