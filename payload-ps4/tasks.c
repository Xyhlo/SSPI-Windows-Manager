#include "tasks.h"
#include "sha256.h"
#include <stdio.h>
#include <string.h>
#include <limits.h>
#include <errno.h>
int task_record_path(const char *cid, enum PkgKind kind, char *path, size_t cap) {
    if (!valid_content_id(cid)||(kind!=PKG_BASE&&kind!=PKG_UPDATE&&kind!=PKG_DLC)) return -1;
    int n=snprintf(path,cap,TASK_ROOT "/%s-%d.rec",cid,kind); return n<0||(size_t)n>=cap?-1:0;
}
static bool valid(const TaskRecord *r) {
    char path[160], hash[65], digest[65];
    if (r->task_id<0||!r->size||r->size>INT64_MAX||task_record_path(r->content_id,r->kind,path,sizeof(path))) return false;
    memcpy(hash,r->header_sha256,sizeof(hash)); memcpy(digest,r->digest,sizeof(digest)); hash[64]=digest[64]=0;
    return (!hash[0]||!hash_normalize(hash))&&(!digest[0]||!hash_normalize(digest));
}
int task_record_format(const TaskRecord *r, char *text, size_t cap) {
    if (!valid(r)) return -1;
    char declared[32]; if (r->has_declared_size) snprintf(declared,sizeof(declared),"%llu",(unsigned long long)r->declared_size); else snprintf(declared,sizeof(declared),"unknown");
    int n=snprintf(text,cap,"SSPI-PS4-TASK 1\ntask_id=%d\ncontent_id=%s\nkind=%d\nsize=%llu\ndeclared_size=%s\nheader_sha256=%s\ndigest=%s\ncreated=%llu\n",
        r->task_id,r->content_id,r->kind,(unsigned long long)r->size,declared,r->header_sha256,r->digest,(unsigned long long)r->created);
    return n<0||(size_t)n>=cap?-1:n;
}
static int field(const char **p, const char *end, const char *key, char *out, size_t cap) {
    size_t n=strlen(key); if ((size_t)(end-*p)<n||memcmp(*p,key,n)) return -1; *p+=n;
    const char *nl=memchr(*p,'\n',(size_t)(end-*p)); if (!nl||(size_t)(nl-*p)>=cap) return -1;
    n=(size_t)(nl-*p); memcpy(out,*p,n); out[n]=0; *p=nl+1; return 0;
}
static int decimal(const char *s, uint64_t *out) {
    if (!*s) return -1; uint64_t n=0;
    for (;*s;s++) { if (*s<'0'||*s>'9') return -1; unsigned d=(unsigned)(*s-'0'); if (n>(UINT64_MAX-d)/10) return -1; n=n*10+d; }
    *out=n; return 0;
}
int task_record_parse(const char *text, size_t size, TaskRecord *r) {
    const char *magic="SSPI-PS4-TASK 1\n";
    if (!text||size<strlen(magic)||size>=1024||memcmp(text,magic,strlen(magic))||memchr(text,0,size)) return -1;
    const char *p=text+strlen(magic), *end=text+size; char value[65]; uint64_t n; memset(r,0,sizeof(*r));
    if (field(&p,end,"task_id=",value,sizeof(value))||decimal(value,&n)||n>INT_MAX) return -1; r->task_id=(int)n;
    if (field(&p,end,"content_id=",r->content_id,sizeof(r->content_id))) return -1;
    if (field(&p,end,"kind=",value,sizeof(value))||decimal(value,&n)||n>INT_MAX) return -1; r->kind=(enum PkgKind)n;
    if (field(&p,end,"size=",value,sizeof(value))||decimal(value,&r->size)) return -1;
    if (field(&p,end,"declared_size=",value,sizeof(value))) return -1;
    if (strcmp(value,"unknown")) { if (decimal(value,&r->declared_size)) return -1; r->has_declared_size=true; }
    if (field(&p,end,"header_sha256=",r->header_sha256,sizeof(r->header_sha256))||field(&p,end,"digest=",r->digest,sizeof(r->digest))) return -1;
    if (field(&p,end,"created=",value,sizeof(value))||decimal(value,&r->created)||p!=end||!valid(r)) return -1;
    if (r->header_sha256[0]&&hash_normalize(r->header_sha256)) return -1;
    if (r->digest[0]&&hash_normalize(r->digest)) return -1;
    return 0;
}
int task_record_save(const TaskRecord *r) {
    char path[160], text[1024]; int n=task_record_format(r,text,sizeof(text));
    if (n<0||task_record_path(r->content_id,r->kind,path,sizeof(path))||rx_mkdir(TASK_ROOT)) return -1;
    return rx_atomic_file(path,text,(size_t)n);
}
int task_record_load(const char *cid, enum PkgKind kind, TaskRecord *r) {
    char path[160], text[1024]; if (task_record_path(cid,kind,path,sizeof(path))) return -1;
    int fd=rx_open(path,RX_READ); if (fd<0) return -1;
    uint64_t n; int rc=rx_size(fd,&n); if (!rc && n<sizeof(text)) rc=rx_read_exact(fd,text,(size_t)n,0); else rc=-1;
    if (rx_close(fd)) rc=-1;
    if (rc||task_record_parse(text,(size_t)n,r)||strcmp(cid,r->content_id)||kind!=r->kind) return -1; return 0;
}
bool task_record_matches(const TaskRecord *r, int task, const UrlRequest *q) {
    if (!valid(r)||r->task_id!=task||strcmp(r->content_id,q->content_id)||r->kind!=q->kind||r->size!=q->size) return false;
    if (r->has_declared_size&&q->has_declared_size&&r->declared_size!=q->declared_size) return false;
    if (r->digest[0]&&q->digest[0]&&strcmp(r->digest,q->digest)) return false;
    if (q->header_sha256[0]) return r->header_sha256[0]&&!strcmp(r->header_sha256,q->header_sha256);
    return q->digest[0]&&r->digest[0]&&!strcmp(r->digest,q->digest);
}
int task_record_remove(const char *cid, enum PkgKind kind, int task) {
    char path[160]; TaskRecord record;
    if (task_record_path(cid,kind,path,sizeof(path))) return -1;
    if (task_record_load(cid,kind,&record)) return -1;
    if (task>=0&&record.task_id!=task) return -1;
    return rx_unlink(path);
}
