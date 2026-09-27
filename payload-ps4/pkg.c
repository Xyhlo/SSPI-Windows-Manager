#include "pkg.h"
#include "sha256.h"
#include <string.h>
#include <stdio.h>
static uint32_t be32(const uint8_t *p) { return ((uint32_t)p[0]<<24)|((uint32_t)p[1]<<16)|((uint32_t)p[2]<<8)|p[3]; }
bool valid_title_id(const char *s) {
    if (!s || strlen(s)!=9 || (memcmp(s,"CUSA",4)&&memcmp(s,"PPSA",4))) return false;
    for (int i=4;i<9;i++) if (s[i]<'0'||s[i]>'9') return false;
    return true;
}
bool valid_content_id(const char *s) {
    if (!s || strlen(s)!=36 || s[6]!='-' || s[16]!='_' || s[19]!='-') return false;
    char tid[10]; memcpy(tid,s+7,9); tid[9]=0;
    if (!valid_title_id(tid) || memcmp(tid,"CUSA",4)) return false;
    for (unsigned i=0;i<36;i++) {
        if (i==6||i==16||i==19) continue;
        if (!((s[i]>='A'&&s[i]<='Z')||(s[i]>='0'&&s[i]<='9')||s[i]=='_')) return false;
    }
    return true;
}
enum PkgKind pkg_kind(const char *s) { return !strcmp(s,"base")?PKG_BASE:!strcmp(s,"update")?PKG_UPDATE:!strcmp(s,"dlc")?PKG_DLC:PKG_OTHER; }
const char *pkg_bgft_type(uint32_t type) {
    switch (type) { case 0x1a:return "PS4GD"; case 0x1b:return "PS4AC"; case 0x1c:return "PS4AL"; case 0x1e:return "PS4DP"; default:return NULL; }
}
int pkg_magic(int fd, uint64_t size) { uint8_t b[4]; return size>=0x80 && !rx_read_exact(fd,b,4,0) && !memcmp(b,"\x7f" "CNT",4) ? 0:-1; }
int pkg_parse(const uint8_t *h, size_t length, uint64_t size, PkgInfo *p) {
    if (length<4096 || size<4096 || memcmp(h,"\x7f" "CNT",4)) return -1;
    memset(p,0,sizeof(*p)); memcpy(p->header,h,4096); memcpy(p->content_id,h+0x40,36);
    if (!valid_content_id(p->content_id)) return -1;
    memcpy(p->title_id,p->content_id+7,9); p->size=size; p->content_type=be32(h+0x74); p->iro_tag=be32(h+0x98);
    /* CNT content_flags at 0x78 carries the full-patch flags (0x1a is also base). */
    uint32_t flags=be32(h+0x78);
    if (p->content_type==0x1a) p->kind=(flags&0x40300000u)?PKG_UPDATE:PKG_BASE;
    else if (p->content_type==0x1b||p->content_type==0x1c) p->kind=PKG_DLC;
    else if (p->content_type==0x1e) p->kind=PKG_UPDATE;
    else return -1;
    return 0;
}
int pkg_read(int fd, PkgInfo *p) { uint8_t h[4096]; uint64_t size; return rx_size(fd,&size)||rx_read_exact(fd,h,4096,0)?-1:pkg_parse(h,4096,size,p); }
int pkg_files_equal(int left, int right, uint64_t size) {
    uint8_t a[32768], b[32768]; uint64_t ls, rs;
    if (rx_size(left,&ls)||rx_size(right,&rs)||ls!=size||rs!=size) return -1;
    for (uint64_t offset=0;offset<size;) {
        size_t n=size-offset>sizeof(a)?sizeof(a):(size_t)(size-offset);
        if (rx_read_exact(left,a,n,offset)||rx_read_exact(right,b,n,offset)) return -1;
        if (memcmp(a,b,n)) return 1; offset+=n;
    }
    return rx_size(left,&ls)||rx_size(right,&rs)||ls!=size||rs!=size?-1:0;
}
int pkg_installed_path(const PkgInfo *p, char *out, size_t cap) {
    if (!valid_content_id(p->content_id)||!valid_title_id(p->title_id)) return -1;
    int n;
    if (p->kind==PKG_BASE) n=snprintf(out,cap,"/user/app/%s/app.pkg",p->title_id);
    else if (p->kind==PKG_UPDATE) n=snprintf(out,cap,"/user/patch/%s/patch.pkg",p->title_id);
    /* Themes (IRO tag 1 or 2) are kept per tag and full content ID, e.g.
       /user/addcont/I00000002/UP9000-CUSA00000_00-LABEL/ac.pkg (seen on 12.02). */
    else if (p->kind==PKG_DLC && (p->iro_tag==1||p->iro_tag==2)) n=snprintf(out,cap,"/user/addcont/I%08u/%s/ac.pkg",(unsigned)p->iro_tag,p->content_id);
    else if (p->kind==PKG_DLC) n=snprintf(out,cap,"/user/addcont/%s/%.16s/ac.pkg",p->title_id,p->content_id+20);
    else return -1;
    return n<0||(size_t)n>=cap?-1:0;
}
int pkg_header_hash(int fd, char hash[65]) {
    uint8_t header[4096]; uint64_t size;
    if (rx_size(fd,&size)) return -1; size_t n=size<sizeof(header)?(size_t)size:sizeof(header);
    if (rx_read_exact(fd,header,n,0)) return -1; sha256_hex(header,n,hash); return 0;
}
int pkg_verify_installed_copy(int source, const PkgInfo *expected, char *path, size_t cap) {
    char internal[160]; if (pkg_installed_path(expected,internal,sizeof(internal))) return -1;
    for (unsigned root=0;root<2;root++) {
        int n=snprintf(path,cap,"%s%s",root?"/mnt/ext0":"",internal); if (n<0||(size_t)n>=cap) return -1;
        int target=rx_open(path,RX_READ); if (target<0) continue;
        PkgInfo actual; int rc=pkg_read(target,&actual);
        if (!rc && (actual.size!=expected->size||memcmp(actual.header,expected->header,4096))) rc=-1;
        if (!rc) rc=pkg_files_equal(source,target,expected->size);
        if (rx_close(target)) rc=-1;
        if (!rc) return 0;
    }
    path[0]=0; return -1;
}
int pkg_find_installed(const PkgInfo *expected, const char *hash, const char *digest,
    PkgInfo *actual, char *path, size_t cap, uint64_t *stamp) {
    char internal[160]; if (pkg_installed_path(expected,internal,sizeof(internal))) return -1;
    for (unsigned root=0;root<2;root++) {
        int n=snprintf(path,cap,"%s%s",root?"/mnt/ext0":"",internal);
        if (n<0||(size_t)n>=cap) return -1;
        uint64_t size; if (rx_stat(path,&size,stamp)||size!=expected->size) continue;
        int fd=rx_open(path,RX_READ); if (fd<0) continue;
        int rc=pkg_read(fd,actual);
        if (rx_close(fd)) rc=-1;
        if (rc||actual->kind!=expected->kind||strcmp(actual->content_id,expected->content_id)) continue;
        char hex[65];
        if (hash&&*hash) { sha256_hex(actual->header,sizeof(actual->header),hex); if (strcmp(hex,hash)) continue; }
        if (digest&&*digest) { digest_hex(actual->header+0xfe0,hex); if (strcmp(hex,digest)) continue; }
        return 0;
    }
    path[0]=0; return -1;
}
