#include "transfer.h"
#include "notify.h"
#include "log.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>

struct Segment { uint64_t offset, length; struct Segment *next; };
struct Transfer {
    char path[MAX_PATH_BYTES+1]; uint64_t total, completed;
    unsigned active_lanes, segment_count; bool verified, pinned;
    Segment *segments; int fd; struct Transfer *next;
};
static RxMutex lock=RX_MUTEX_INIT;
static Transfer *transfers;
static unsigned transfer_count, upload_lanes;
bool allowed_path(const char *p) {
    const char *prefix=UPLOAD_ROOT "/upload_";
    if (!p) return false;
    size_t n=strlen(p), k=strlen(prefix);
    if (n>MAX_PATH_BYTES||n<=k+4||strncmp(p,prefix,k)||strcmp(p+n-4,".pkg")) return false;
    for (size_t i=k;i<n-4;i++) if (!((p[i]>='A'&&p[i]<='Z')||(p[i]>='a'&&p[i]<='z')||
        (p[i]>='0'&&p[i]<='9')||p[i]=='_'||p[i]=='-')) return false;
    return true;
}
static Transfer *find(const char *p) { for (Transfer *t=transfers;t;t=t->next) if (!strcmp(t->path,p)) return t; return NULL; }
static void destroy(Transfer *t) {
    if (t->active_lanes||t->pinned) return;
    Transfer **p=&transfers; while (*p && *p!=t) p=&(*p)->next; if (!*p) return;
    *p=t->next; if (rx_close(t->fd)) log_line("transfer close failed: %s",t->path);
    while (t->segments) { Segment *s=t->segments; t->segments=s->next; free(s); }
    free(t); transfer_count--;
}
bool transfer_busy(void) {
    rx_lock(&lock); bool busy=upload_lanes!=0; rx_unlock(&lock); return busy;
}
void release_lane(Lane *lane) {
    rx_lock(&lock); Transfer *t=lane->transfer;
    if (t) { t->active_lanes--; upload_lanes--; memset(lane,0,sizeof(*lane)); if (!t->active_lanes && t->completed!=t->total) destroy(t); }
    rx_unlock(&lock);
}
int handle_create_dir(int fd, const char *p) {
    if (!p||strcmp(p,UPLOAD_ROOT)) return text_reply(fd,RESP_ERROR,"path rejected");
    return text_reply(fd,rx_mkdir(p)?RESP_ERROR:RESP_OK,"CREATE_DIR");
}
int handle_start(int fd, const uint8_t *body, uint32_t size, Lane *lane) {
    if (lane->transfer) return text_reply(fd,RESP_ERROR,"lane already started");
    const uint8_t *nul=body?memchr(body,0,size):NULL;
    if (!nul || nul-body>MAX_PATH_BYTES || (uint64_t)(nul-body)+25!=size) return text_reply(fd,RESP_ERROR,"invalid START_UPLOAD");
    const char *path=(const char *)body;
    if (!allowed_path(path)) return text_reply(fd,RESP_ERROR,"path rejected");
    uint64_t total=read_u64le(nul+1), offset=read_u64le(nul+9), segment=read_u64le(nul+17);
    if (!total||total>INT64_MAX||offset>total||!segment||segment>total-offset) return text_reply(fd,RESP_ERROR,"invalid segment");
    const char *error=NULL; bool incoming=false;
    rx_lock(&lock);
    Transfer *t=find(path);
    if (upload_lanes>=MAX_UPLOAD_LANES) { error="receiver upload capacity busy; retry"; goto done; }
    if (t && (t->pinned || (offset==0 && t->active_lanes))) { error="previous attempt still closing or installing; retry"; goto done; }
    if (t && offset==0) { destroy(t); t=NULL; }
    if (!t) {
        if (transfer_count>=MAX_TRANSFERS) { error="receiver transfer capacity busy"; goto done; }
        if (offset) { error="lane 0 must create file first"; goto done; }
        if (rx_mkdir(UPLOAD_ROOT)) { error="upload directory unavailable"; goto done; }
        int file=rx_open(path,RX_CREATE); if (file<0) { error="create failed or symlink rejected"; goto done; }
        if (rx_resize(file,total)) { if (rx_close(file)) log_line("close after preallocate failed"); error="preallocate failed (console storage full?)"; goto done; }
        t=calloc(1,sizeof(*t));
        if (!t) { if (rx_close(file)) log_line("close after allocation failed"); error="out of memory"; goto done; }
        strcpy(t->path,path); t->total=total; t->fd=file; t->next=transfers; transfers=t; transfer_count++; incoming=true;
    }
    if (t->total!=total) { error="total size mismatch"; goto done; }
    if (t->verified) { error="transfer already verified"; goto done; }
    if (t->segment_count>=1024) { error="too many segments"; goto done; }
    for (Segment *s=t->segments;s;s=s->next) if (offset<s->offset+s->length && s->offset<offset+segment) { error="overlapping segment"; goto done; }
    Segment *s=calloc(1,sizeof(*s)); if (!s) { error="out of memory"; goto done; }
    s->offset=offset; s->length=segment; s->next=t->segments; t->segments=s; t->segment_count++;
    t->active_lanes++; upload_lanes++; lane->transfer=t; lane->segment=s;
    lane->offset=offset; lane->expected=segment; lane->received=0;
done:
    rx_unlock(&lock);
    if (!error && incoming) notify_receiving(path);
    return text_reply(fd,error?RESP_ERROR:RESP_READY,error?error:"READY");
}
int handle_upload_chunk(int fd, uint32_t size, Lane *lane, uint8_t *buffer) {
    ReceiveDeadline deadline; receive_begin(&deadline,size);
    return handle_upload_chunk_deadline(fd,size,lane,buffer,&deadline);
}
int handle_upload_chunk_deadline(int fd, uint32_t size, Lane *lane, uint8_t *buffer, ReceiveDeadline *deadline) {
    if (!lane->transfer||!size||size>MAX_FRAME||(uint64_t)size>lane->expected-lane->received) {
        if (text_reply(fd,RESP_ERROR,"invalid chunk")) return -1;
        return -1;
    }
    uint32_t left=size;
    while (left) {
        size_t n=left<UPLOAD_BUFFER?left:UPLOAD_BUFFER;
        if (recv_all_deadline(fd,buffer,n,deadline)) return -1;
        if (rx_write_exact(lane->transfer->fd,buffer,n,lane->offset+lane->received)) {
            if (text_reply(fd,RESP_ERROR,"pwrite failed")) return -1;
            return -1;
        }
        lane->received+=n; left-=(uint32_t)n;
    }
    return text_reply(fd,RESP_OK,"OK");
}
int handle_end(int fd, Lane *lane) {
    if (!lane->transfer||lane->received!=lane->expected) return text_reply(fd,RESP_ERROR,"segment incomplete");
    if (rx_sync(lane->transfer->fd)) return text_reply(fd,RESP_ERROR,"fsync failed");
    rx_lock(&lock); Transfer *t=lane->transfer; t->completed+=lane->expected; t->active_lanes--; upload_lanes--;
    memset(lane,0,sizeof(*lane)); rx_unlock(&lock);
    return text_reply(fd,RESP_OK,"OK");
}
int handle_verify(int fd, const char *path) {
    if (!allowed_path(path)) return text_reply(fd,RESP_ERROR,"path rejected");
    rx_lock(&lock); Transfer *t=find(path); uint64_t size=0; const char *error=NULL;
    if (!t||t->active_lanes||t->completed!=t->total||rx_size(t->fd,&size)||size!=t->total) error="file unavailable or transfer incomplete";
    else if (pkg_magic(t->fd,size)) error="PKG magic failed";
    else t->verified=true;
    rx_unlock(&lock);
    char msg[80]; snprintf(msg,sizeof(msg),"OK {\"size\":%llu}",(unsigned long long)size);
    return text_reply(fd,error?RESP_ERROR:RESP_OK,error?error:msg);
}
int transfer_pin(const char *path, PkgInfo *info) {
    if (!allowed_path(path)) return -1;
    rx_lock(&lock); Transfer *t=find(path); int rc=-1;
    if (t && t->verified && !t->active_lanes && !t->pinned && !pkg_read(t->fd,info) && info->size==t->total) { t->pinned=true; rc=0; }
    rx_unlock(&lock); return rc;
}
void transfer_finish(const char *path, bool installed) {
    rx_lock(&lock); Transfer *t=find(path);
    if (t) { t->pinned=false; if (installed) { destroy(t); if (rx_unlink(path)) log_line("installed upload cleanup failed: %s",path); } }
    rx_unlock(&lock);
}
void transfer_reset(void) {
    rx_lock(&lock); Transfer *t=transfers;
    while (t) { Transfer *next=t->next; destroy(t); t=next; }
    rx_unlock(&lock);
}
