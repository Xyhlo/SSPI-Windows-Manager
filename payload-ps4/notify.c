#include "notify.h"
#include "proto.h"
#include "json.h"
#include "log.h"
#include <string.h>
#include <stdio.h>
static RxMutex lock=RX_MUTEX_INIT, artwork_lock=RX_MUTEX_INIT;
static TitleContext context;
typedef struct { char id[MAX_PATH_BYTES+1]; unsigned events; } Seen;
typedef struct { char tid[10], text[512]; } Notice;
static Seen seen[128]; static unsigned next_seen;
static Notice queue[128]; static unsigned head, count;
static uint64_t last_toast;
int parse_context(const uint8_t *b, size_t n, TitleContext *c) {
    if (!b||n<13||b[9]) return -1;
    if (!valid_title_id((const char *)b)) return -1;
    const uint8_t *name=b+10, *end=memchr(name,0,n-10);
    if (!end||end==name||end-name>=220) return -1;
    size_t remain=n-(size_t)(end+1-b); const uint8_t *icon=end+1;
    if (!remain||remain>1000||icon[remain-1]||memchr(icon,0,remain-1)) return -1;
    if (*icon&&strncmp((const char *)icon,"http://",7)&&strncmp((const char *)icon,"https://",8)) return -1;
    memset(c,0,sizeof(*c)); memcpy(c->title_id,b,9); memcpy(c->title,name,(size_t)(end-name)); memcpy(c->icon_url,icon,remain); return 0;
}
int parse_artwork(const uint8_t *b, size_t n, Artwork *a) {
    if (!b||n<18||b[0]>1||b[10]||!valid_title_id((const char *)b+1)) return -1;
    uint32_t length=read_u32le(b+11);
    if (length>512u*1024u||length>n-18) return -1;
    const uint8_t *image=b+15, *json=image+length; size_t jn=n-15-length;
    if (jn>16384||json[jn-1]||memchr(json,0,jn-1)||json_object_valid((const char *)json,jn-1)) return -1;
    if (length && !(length>=8&&!memcmp(image,"\x89PNG\r\n\x1a\n",8)) &&
        !(length>=3&&image[0]==0xff&&image[1]==0xd8&&image[2]==0xff)) return -1;
    memcpy(a->title_id,b+1,10); a->image=image; a->image_size=length; return 0;
}
int notify_context(const uint8_t *b, size_t n) {
    TitleContext c; if (parse_context(b,n,&c)) return -1;
    rx_lock(&lock); context=c; rx_unlock(&lock); return 0;
}
void notify_get_context(TitleContext *c) { rx_lock(&lock); *c=context; rx_unlock(&lock); }
int notify_artwork(const uint8_t *b, size_t n) {
    Artwork a; if (parse_artwork(b,n,&a)) return -1; if (!a.image_size) return 0;
    char path[128]; snprintf(path,sizeof(path),ART_ROOT "/%s.png",a.title_id);
    rx_lock(&artwork_lock); int rc=rx_mkdir(ART_ROOT); if (!rc) rc=rx_atomic_file(path,a.image,a.image_size); rx_unlock(&artwork_lock);
    if (rc) log_line("artwork cache failed: %s",a.title_id); return rc;
}
static void enqueue(const char *tid, const char *text) {
    if (count==128) { log_line("notification queue full: %s",text); return; }
    Notice *n=&queue[(head+count)%128]; snprintf(n->tid,sizeof(n->tid),"%s",tid?tid:""); snprintf(n->text,sizeof(n->text),"%s",text); count++;
}
void notify_system(const char *text) { rx_lock(&lock); enqueue("",text); rx_unlock(&lock); }
void notify_event(const char *id, const char *tid, const char *title, unsigned event, const char *error) {
    rx_lock(&lock); Seen *s=NULL;
    for (unsigned i=0;i<128;i++) if (!strcmp(seen[i].id,id)) { s=&seen[i]; break; }
    if (!s) { s=&seen[next_seen++%128]; snprintf(s->id,sizeof(s->id),"%s",id); s->events=0; }
    if (s->events&event) { rx_unlock(&lock); return; }
    s->events|=event;
    char text[512]; const char *prefix=event==NOTICE_RECEIVING?"Receiving":event==NOTICE_INSTALLING?"Installing":event==NOTICE_INSTALLED?"Installed":"Install failed:";
    snprintf(text,sizeof(text),"%s %s%s%.160s",prefix,title&&*title?title:tid,event==NOTICE_FAILED?" \xe2\x80\x94 ":"",event==NOTICE_FAILED&&error?error:"");
    enqueue(tid,text); rx_unlock(&lock);
}
void notify_receiving(const char *path) {
    TitleContext c; notify_get_context(&c);
    if (!c.title_id[0]) { const char *p=strstr(path,"upload_"); if (p&&strlen(p+7)>=9) { memcpy(c.title_id,p+7,9); c.title_id[9]=0; if (!valid_title_id(c.title_id)) c.title_id[0]=0; } }
    if (!c.title[0]) snprintf(c.title,sizeof(c.title),"%s",c.title_id[0]?c.title_id:"package");
    notify_event(path,c.title_id,c.title,NOTICE_RECEIVING,"");
}
void notify_tick(void) {
    uint64_t now=rx_now(); rx_lock(&lock);
    if (!count||(last_toast && now-last_toast<3000)) { rx_unlock(&lock); return; }
    Notice n=queue[head]; head=(head+1)%128; count--; last_toast=now?now:1; rx_unlock(&lock);
    int rc=platform_toast(n.tid,n.text); log_line("toast rc=0x%08x %s",(unsigned)rc,n.text);
}
