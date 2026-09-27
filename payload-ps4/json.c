#include "json.h"
#include "proto.h"
#include "log.h"
#include "sha256.h"
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <limits.h>

void json_init(Json *j, char *p, size_t n) { *j=(Json){p,n,0,n==0}; if (n) p[0]=0; }
void json_add(Json *j, const char *fmt, ...) {
    if (j->failed) return;
    va_list a; va_start(a,fmt); int n=vsnprintf(j->data+j->used,j->cap-j->used,fmt,a); va_end(a);
    if (n<0||(size_t)n>=j->cap-j->used) j->failed=true; else j->used+=(size_t)n;
}
void json_quote(Json *j, const char *s) {
    json_add(j,"\"");
    for (const unsigned char *p=(const unsigned char *)(s?s:"");*p;p++) {
        if (*p=='"'||*p=='\\') json_add(j,"\\%c",*p);
        else if (*p<32) json_add(j,"\\u%04x",*p);
        else json_add(j,"%c",*p);
    }
    json_add(j,"\"");
}
typedef struct { const char *p,*end; } Parser;
static void ws(Parser *p) { while (p->p<p->end && (*p->p==' '||*p->p=='\t'||*p->p=='\r'||*p->p=='\n')) p->p++; }
static int hex4(Parser *p, uint32_t *v) {
    *v=0; for (int i=0;i<4;i++) { if (p->p==p->end) return -1; unsigned c=(unsigned char)*p->p++; unsigned x=c>='0'&&c<='9'?c-'0':c>='a'&&c<='f'?c-'a'+10:c>='A'&&c<='F'?c-'A'+10:16; if (x>15) return -1; *v=(*v<<4)|x; } return 0;
}
static int put(char *out, size_t cap, size_t *n, unsigned c) { if (out) { if (*n+1>=cap) return -1; out[*n]=(char)c; } (*n)++; return 0; }
static int string(Parser *p, char *out, size_t cap) {
    ws(p); if (p->p==p->end||*p->p++!='"') return -1; size_t n=0;
    while (p->p<p->end) {
        unsigned c=(unsigned char)*p->p++;
        if (c=='"') { if (out) out[n]=0; return 0; }
        if (c<32) return -1;
        if (c=='\\') {
            if (p->p==p->end) return -1; c=(unsigned char)*p->p++;
            switch(c) {
                case '"': case '\\': case '/': break;
                case 'b': c=8; break; case 'f': c=12; break; case 'n': c=10; break; case 'r': c=13; break; case 't': c=9; break;
                case 'u': {
                    uint32_t u; if (hex4(p,&u)||!u) return -1;
                    if (u>=0xd800&&u<=0xdbff) { uint32_t low; if (p->end-p->p<6||p->p[0]!='\\'||p->p[1]!='u') return -1; p->p+=2; if (hex4(p,&low)||low<0xdc00||low>0xdfff) return -1; u=0x10000+((u-0xd800)<<10)+(low-0xdc00); }
                    else if (u>=0xdc00&&u<=0xdfff) return -1;
                    if (u>=0x10000 && put(out,cap,&n,0xf0|(u>>18))) return -1;
                    if (u>=0x800 && put(out,cap,&n,(u>=0x10000?0x80:0xe0)|((u>>12)&(u>=0x10000?63:15)))) return -1;
                    if (u>=0x80 && put(out,cap,&n,(u>=0x800?0x80:0xc0)|((u>>6)&(u>=0x800?63:31)))) return -1;
                    c=u<0x80?u:0x80|(u&63); break;
                }
                default: return -1;
            }
        }
        if (put(out,cap,&n,c)) return -1;
    }
    return -1;
}
static int number(Parser *p, uint64_t *integer) {
    ws(p); const char *start=p->p; bool negative=false, fraction=false; uint64_t v=0;
    if (p->p<p->end && *p->p=='-') { negative=true; p->p++; }
    if (p->p==p->end || *p->p<'0'||*p->p>'9') return -1;
    bool zero=*p->p=='0'; unsigned digits=0;
    while (p->p<p->end && *p->p>='0'&&*p->p<='9') { unsigned d=(unsigned)(*p->p++-'0'); if (integer && v>(UINT64_MAX-d)/10) return -1; v=v*10+d; digits++; }
    if (zero&&digits!=1) return -1;
    if (p->p<p->end&&*p->p=='.') { fraction=true; p->p++; const char *a=p->p; while (p->p<p->end&&*p->p>='0'&&*p->p<='9') p->p++; if (a==p->p) return -1; }
    if (p->p<p->end&&(*p->p=='e'||*p->p=='E')) { fraction=true; p->p++; if (p->p<p->end&&(*p->p=='+'||*p->p=='-')) p->p++; const char *a=p->p; while (p->p<p->end&&*p->p>='0'&&*p->p<='9') p->p++; if (a==p->p) return -1; }
    if (integer) { if (negative||fraction) return -1; *integer=v; }
    return p->p==start?-1:0;
}
static int value(Parser *p, unsigned depth) {
    if (depth>16) return -1; ws(p); if (p->p==p->end) return -1;
    if (*p->p=='"') return string(p,NULL,0);
    if (*p->p=='{'||*p->p=='[') {
        bool object=*p->p++=='{'; char end=object?'}':']'; ws(p);
        if (p->p<p->end&&*p->p==end) { p->p++; return 0; }
        for (;;) {
            if (object) { if (string(p,NULL,0)) return -1; ws(p); if (p->p==p->end||*p->p++!=':') return -1; }
            if (value(p,depth+1)) return -1; ws(p); if (p->p==p->end) return -1;
            char c=*p->p++; if (c==end) return 0; if (c!=',') return -1;
        }
    }
    const char *words[]={"true","false","null"};
    for (int i=0;i<3;i++) { size_t n=strlen(words[i]); if ((size_t)(p->end-p->p)>=n&&!memcmp(p->p,words[i],n)) { p->p+=n; return 0; } }
    return number(p,NULL);
}
int json_object_valid(const char *s, size_t n) {
    Parser p={s,s+n}; ws(&p); if (p.p==p.end||*p.p!='{'||value(&p,0)) return -1; ws(&p); return p.p==p.end?0:-1;
}
static bool http_url(const char *s, bool https) {
    size_t n=!strncmp(s,"http://",7)?7:https&&!strncmp(s,"https://",8)?8:0;
    if (!n||!s[n]||s[n]=='/'||s[n]==':'||s[n]=='?'||s[n]=='#') return false;
    for (const unsigned char *p=(const unsigned char *)s;*p;p++) if (*p<=32||*p==127||*p=='\\') return false;
    return true;
}
int parse_install_url(const uint8_t *body, size_t size, UrlRequest *r, char *error, size_t cap) {
    const char *s=wire_string(body,size,8192); const char *reason="invalid INSTALL_URL JSON";
    if (!s) goto bad;
    memset(r,0,sizeof(*r)); Parser p={s,s+size-1}; ws(&p); if (p.p==p.end||*p.p++!='{') goto bad;
    unsigned fields=0; char kind[16];
    for (;;) {
        char key[32]; if (string(&p,key,sizeof(key))) goto bad;
        ws(&p); if (p.p==p.end||*p.p++!=':') goto bad;
        unsigned bit; char *out; size_t capacity;
        if (!strcmp(key,"url")) {bit=1;out=r->url;capacity=sizeof(r->url);}
        else if (!strcmp(key,"content_id")) {bit=2;out=r->content_id;capacity=sizeof(r->content_id);}
        else if (!strcmp(key,"kind")) {bit=4;out=kind;capacity=sizeof(kind);}
        else if (!strcmp(key,"title")) {bit=8;out=r->title;capacity=sizeof(r->title);}
        else if (!strcmp(key,"title_id")) {bit=16;out=r->title_id;capacity=sizeof(r->title_id);}
        else if (!strcmp(key,"icon_url")) {bit=32;out=r->icon_url;capacity=sizeof(r->icon_url);}
        else if (!strcmp(key,"size")) {bit=64;out=NULL;capacity=0;}
        else if (!strcmp(key,"declared_size")) {bit=128;out=NULL;capacity=0;}
        else if (!strcmp(key,"content_type")) {bit=256;out=NULL;capacity=0;}
        else if (!strcmp(key,"digest")) {bit=512;out=r->digest;capacity=sizeof(r->digest);}
        else if (!strcmp(key,"header_sha256")) {bit=1024;out=r->header_sha256;capacity=sizeof(r->header_sha256);}
        else goto bad;
        if (fields&bit) goto bad; fields|=bit;
        if (out) { if (string(&p,out,capacity)) goto bad; }
        else {
            uint64_t n; if (number(&p,&n)) goto bad;
            if (bit==64) r->size=n;
            else if (bit==128) { r->declared_size=n; r->has_declared_size=true; }
            else { if (n>UINT32_MAX) goto bad; r->content_type=(uint32_t)n; r->has_content_type=true; }
        }
        ws(&p); if (p.p==p.end) goto bad; char c=*p.p++; if (c=='}') break; if (c!=',') goto bad;
    }
    ws(&p); if (p.p!=p.end||(fields&127)!=127) goto bad;
    reason="url must be HTTP; icon_url must be empty or HTTP(S)";
    if (!http_url(r->url,false)||(r->icon_url[0]&&!http_url(r->icon_url,true))) goto bad;
    reason="invalid or mismatched PS4 content ID/title ID";
    if (!valid_content_id(r->content_id)||!valid_title_id(r->title_id)||memcmp(r->content_id+7,r->title_id,9)) goto bad;
    reason="kind must be base, update or dlc; title and size are required";
    r->kind=pkg_kind(kind); if (!r->kind||!r->title[0]||!r->size||r->size>INT64_MAX) goto bad;
    reason="digest and header_sha256 must contain exactly 64 hexadecimal characters";
    if (((fields&512)&&hash_normalize(r->digest))||((fields&1024)&&hash_normalize(r->header_sha256))) goto bad;
    reason="unsupported CNT content_type";
    if (r->has_content_type&&!pkg_bgft_type(r->content_type)) goto bad;
    return 0;
bad:
    if (cap) snprintf(error,cap,"%s",reason); return -1;
}
int submission_json(char *out, size_t cap, int rc, const char *cid, const char *path, const char *error, int task, bool url) {
    Json j; json_init(&j,out,cap); json_add(&j,"{\"api_code\":%d,\"install_api_code\":%d,\"auth_restore_code\":0,\"state\":\"%s\",\"content_id\":",rc,rc,rc?"failed":"submitted");
    json_quote(&j,cid); json_add(&j,",\"path\":"); json_quote(&j,path); json_add(&j,",\"error\":"); json_quote(&j,error);
    if (url) json_add(&j,",\"task_id\":%d",task); json_add(&j,"}"); return j.failed?-1:0;
}
int status_json(char *out, size_t cap, const InstallStatus *s) {
    Json j; json_init(&j,out,cap); json_add(&j,"{\"api_code\":%d,\"status_api_code\":%d,\"auth_restore_code\":0,\"state\":",s->api_code,s->api_code); json_quote(&j,s->state);
    json_add(&j,",\"content_id\":"); json_quote(&j,s->content_id); json_add(&j,",\"status\":"); json_quote(&j,s->status);
    int progress=s->progress<0?0:s->progress>100?100:s->progress;
    json_add(&j,",\"progress\":%d,\"downloaded\":%llu,\"total\":%llu,\"error_code\":%d,\"error\":",progress,(unsigned long long)s->downloaded,(unsigned long long)s->total,s->error_code);
    json_quote(&j,s->error); json_add(&j,"}"); return j.failed?-1:0;
}
int preflight_error_json(char *out, size_t cap, int rc, const char *stage, const char *error) {
    Json j; json_init(&j,out,cap); json_add(&j,"{\"api_code\":%d,\"state\":\"failed\",\"stage\":",rc); json_quote(&j,stage); json_add(&j,",\"path\":\"\",\"error\":"); json_quote(&j,error); json_add(&j,"}"); return j.failed?-1:0;
}
int privilege_error_json(char *out, size_t cap, bool jailbroken, int boot, int jbc) {
    if (jailbroken&&!boot&&!jbc) return 0;
    char error[256];
    snprintf(error,sizeof(error),"Receiver lacks system privileges: boot=0x%08x, libjbc=0x%08x, jailbroken=%s. Reload it after enabling GoldHEN.",(unsigned)boot,(unsigned)jbc,jailbroken?"true":"false");
    int rc=boot?boot:jbc?jbc:-1;
    return preflight_error_json(out,cap,rc,"privileges",error)?-1:1;
}
int config_json(char *out, size_t cap, const ReceiverConfig *c) {
    Json j; json_init(&j,out,cap);
    json_add(&j,"{\"port\":%d,\"version\":\"" VERSION "\",\"platform\":\"ps4\",\"uid\":%d,\"jailbroken\":%s,\"data_root\":\"" DATA_ROOT "\",\"writable\":%s,\"bgft\":",c->port,c->uid,c->jailbroken?"true":"false",c->writable?"true":"false");
    json_quote(&j,c->bgft); json_add(&j,",\"appinst\":"); json_quote(&j,c->appinst); json_add(&j,",\"userservice\":"); json_quote(&j,c->userservice);
    json_add(&j,",\"capabilities\":[\"pkg-preflight\",\"pkg-install\",\"url-install\",\"parallel-upload\",\"verify\",\"title-context\",\"progress-notifications\",\"install-control\",\"stop\",\"ps4\",\"installed-library-v1\"],\"diagnostics\":");
    diagnostics_json(&j); json_add(&j,"}"); return j.failed?-1:0;
}
