#define _GNU_SOURCE
#include "install.h"
#include "installed_library.h"
#include "proto.h"
#include "transfer.h"
#include "notify.h"
#include "log.h"
#include "tasks.h"
#include "sha256.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#ifdef SSPI_HOST_TEST
#include "tests/host-sdk.h"
#else
#include <sys/statfs.h>
#include <sched.h>
#include <orbis/libkernel.h>
#endif

static struct {
    int (*app_init)(void), (*app_pkg)(const char *,void *), (*app_exists)(const char *,int *);
    int (*user_init)(void *), (*foreground)(int *), (*bgft_init)(void *);
    int (*reg)(GsBgftParam *,int *), (*debug_reg)(GsBgftParam *,int *);
    int (*start)(int), (*int_start)(int), (*find)(const char *,int,int *);
    int (*progress)(int,GsBgftProgress *), (*pause)(int), (*resume)(int), (*stop)(int), (*unregister_task)(int);
    /* Optional theme controls; their signatures are inferred, so every call is logged. */
    int (*theme_uninstall)(const char *), (*theme_get)(int,char *,size_t), (*theme_set)(int,const char *);
} api;
static ModuleStatus modules={"untried","untried","untried"};
static RxMutex module_lock=RX_MUTEX_INIT, job_lock=RX_MUTEX_INIT, command_lock=RX_MUTEX_INIT;
static int init_code=-1, user_id=-1;
static bool privilege_ready;
static int privilege_boot=-1, privilege_jbc;
static void *bgft_heap;
enum { JOB_QUEUED, JOB_DOWNLOADING, JOB_PAUSED, JOB_INSTALLING, JOB_INSTALLED, JOB_FAILED, JOB_UNCONFIRMED };
typedef struct {
    bool used, local, has_header, baseline_exists, paused, moved;
    int task, phase, error, progress, proof_count;
    char title[220], path[MAX_PATH_BYTES+1], error_text[256], proof_path[192], baseline_path[192];
    UrlRequest request; PkgInfo pkg;
    uint64_t created, updated, movement, downloaded, total, baseline_stamp, proof_stamp, proof_checked;
    uint8_t baseline_digest[32];
    GsBgftProgress last;
} Job;
static Job jobs[MAX_JOBS];
typedef struct {
    bool busy, done, abandoned;
    unsigned command; UrlRequest url; char path[MAX_PATH_BYTES+1], cid[37];
    char result[65536]; uint8_t code;
} Command;
static Command pending;
static uint64_t next_poll;
void install_set_privileges(bool jailbroken, int boot, int jbc) { privilege_ready=jailbroken; privilege_boot=boot; privilege_jbc=jbc; }

static int bind_symbol(int module, const char *name, void *slot, bool required) {
    void *symbol=NULL; int rc=module<0?module:sceKernelDlsym(module,name,&symbol);
    if (!rc&&!symbol) rc=-1;
    memcpy(slot,&symbol,sizeof(symbol));
    diagnostic("export %s rc=0x%08x%s",name,(unsigned)rc,required?" required":" optional");
    return rc;
}
static int scan_module(const char *stem) {
    OrbisKernelModule handles[256]; size_t count=0;
    int rc=sceKernelGetModuleList(handles,sizeof(handles)/sizeof(handles[0]),&count);
    diagnostic("module scan %s rc=0x%08x count=%u",stem,(unsigned)rc,(unsigned)count);
    if (rc) return rc;
    if (count>256) count=256;
    for (size_t i=0;i<count;i++) {
        OrbisKernelModuleInfo info; memset(&info,0,sizeof(info)); info.size=sizeof(info);
        rc=sceKernelGetModuleInfo(handles[i],&info);
        if (rc) { log_line("module info handle=%d rc=0x%08x",handles[i],(unsigned)rc); continue; }
        info.name[sizeof(info.name)-1]=0; const char *base=strrchr(info.name,'/'); base=base?base+1:info.name;
        size_t n=strlen(stem);
        if (!strncmp(base,stem,n) && (!base[n]||!strcmp(base+n,".sprx"))) return handles[i];
    }
    return -1;
}
static int module_load(const char *stem, bool bgft) {
    const char *formats[]={"%s.sprx","/system/common/lib/%s.sprx","/common/lib/%s.sprx"};
    int handle=scan_module(stem); if (handle>=0) return handle;
    for (unsigned i=0;i<3;i++) {
        char path[160]; snprintf(path,sizeof(path),formats[i],stem); int result=0;
        handle=(int)sceKernelLoadStartModule(path,0,NULL,0,NULL,&result);
        diagnostic("module load %s handle=0x%08x start=0x%08x",path,(unsigned)handle,(unsigned)result);
        if (handle>=0 && result>=0) return handle;
        if (handle>=0) return result;
    }
    if (bgft) {
        int sys=module_load("libSceSysmodule",false);
        int (*load)(unsigned)=NULL, (*get_handle)(unsigned,int *)=NULL;
        int a=bind_symbol(sys,"sceSysmoduleLoadModuleInternal",&load,true);
        int b=bind_symbol(sys,"sceSysmoduleGetModuleHandleInternal",&get_handle,true);
        if (!a&&!b) {
            int rc=load(0x8000002a); diagnostic("sysmodule BGFT id=0x8000002a rc=0x%08x",(unsigned)rc);
            int found=-1; rc=get_handle(0x8000002a,&found);
            diagnostic("sysmodule BGFT handle rc=0x%08x handle=%d",(unsigned)rc,found);
            if (!rc&&found>=0) return found;
            found=scan_module(stem); if (found>=0) return found;
            int result=0; found=(int)sceKernelLoadStartModule("libSceBgft.sprx",0,NULL,0,NULL,&result);
            diagnostic("sysmodule BGFT reload handle=0x%08x start=0x%08x",(unsigned)found,(unsigned)result);
            if (found>=0&&result>=0) return found;
        }
    }
    return handle;
}
static void module_state(char *field, int rc) { if (!rc) snprintf(field,40,"ready"); else snprintf(field,40,"unavailable:0x%08x",(unsigned)rc); }
void install_modules(ModuleStatus *s) { rx_lock(&module_lock); *s=modules; rx_unlock(&module_lock); }
int install_init(void) {
    int bgft=module_load("libSceBgft",true), app=module_load("libSceAppInstUtil",false), users=module_load("libSceUserService",false);
    int ar=app<0?app:0, ur=users<0?users:0, br=bgft<0?bgft:0, rc;
#define BIND(mod, name, field, result) do { rc=bind_symbol(mod,name,&api.field,true); if (rc && !(result)) (result)=rc; } while(0)
    BIND(app,"sceAppInstUtilInitialize",app_init,ar);
    BIND(app,"sceAppInstUtilAppInstallPkg",app_pkg,ar);
    BIND(app,"sceAppInstUtilAppExists",app_exists,ar);
    BIND(users,"sceUserServiceGetForegroundUser",foreground,ur);
    BIND(users,"sceUserServiceInitialize",user_init,ur);
    BIND(bgft,"sceBgftServiceIntInit",bgft_init,br);
    BIND(bgft,"sceBgftServiceIntDownloadRegisterTask",reg,br);
    BIND(bgft,"sceBgftServiceIntDebugDownloadRegisterPkg",debug_reg,br);
    BIND(bgft,"sceBgftServiceDownloadFindTaskByContentId",find,br);
    BIND(bgft,"sceBgftServiceDownloadGetProgress",progress,br);
    BIND(bgft,"sceBgftServiceDownloadPauseTask",pause,br);
    BIND(bgft,"sceBgftServiceDownloadResumeTask",resume,br);
    BIND(bgft,"sceBgftServiceDownloadStopTask",stop,br);
    BIND(bgft,"sceBgftServiceIntDownloadUnregisterTask",unregister_task,br);
#undef BIND
    if (bind_symbol(app,"sceAppInstUtilAppUnInstallTheme",&api.theme_uninstall,false)) api.theme_uninstall=NULL;
    if (bind_symbol(users,"sceUserServiceGetThemeEntitlementId",&api.theme_get,false)) api.theme_get=NULL;
    if (bind_symbol(users,"sceUserServiceSetThemeEntitlementId",&api.theme_set,false)) api.theme_set=NULL;
    int sr=bind_symbol(bgft,"sceBgftServiceDownloadStartTask",&api.start,false);
    int ir=bind_symbol(bgft,"sceBgftServiceIntDownloadStartTask",&api.int_start,false);
    if (sr&&ir&&!br) br=sr;
    if (!ar) { ar=api.app_init(); diagnostic("init AppInstUtil rc=0x%08x",(unsigned)ar); }
    if (!ur) {
        ur=api.foreground(&user_id); diagnostic("foreground user rc=0x%08x user=%d",(unsigned)ur,user_id);
        if (ur) {
            int r=api.user_init(NULL); diagnostic("init UserService rc=0x%08x",(unsigned)r);
            ur=api.foreground(&user_id); diagnostic("foreground user retry rc=0x%08x user=%d",(unsigned)ur,user_id);
        }
        if (!ur&&user_id<0) ur=-1;
    }
    if (!br) {
        if (ar||ur) br=ar?ar:ur;
        else {
            bgft_heap=calloc(1,1024u*1024u);
            if (!bgft_heap) br=-ENOMEM;
            else {
                struct { void *heap; size_t size; } heap={bgft_heap,1024u*1024u};
                br=api.bgft_init(&heap); diagnostic("init BGFT heap=1048576 rc=0x%08x",(unsigned)br);
                if ((uint32_t)br==0x80990001u) br=0;
            }
        }
    }
    rx_lock(&module_lock); module_state(modules.appinst,ar); module_state(modules.userservice,ur); module_state(modules.bgft,br);
    init_code=ar?ar:ur?ur:br; rx_unlock(&module_lock);
    diagnostic("installer ready=%d rc=0x%08x",init_code==0,(unsigned)init_code); return init_code;
}
const char *install_error(int code) {
    switch ((uint32_t)code) {
        case 0: return "";
        case 0x80a30002: case 0x80990039: return "not enough free space on the PS4";
        case 0x80a30004: case 0x80a3000a: return "base game is not committed; install the base first";
        case 0x80a30006: return "add-on DRM rejected";
        case 0x80a3000b: return "broken add-on; retry through BGFT URL install";
        case 0x80a3000c: return "close the running game before installing";
        case 0x80990004: return "BGFT rejected registration arguments";
        case 0x80990015: case 0x80990086: return "existing download task conflicts with this package";
        case 0x80990019: return "BGFT task not found";
        case 0x8099002c: return "PS4 could not receive package data from the PC";
        case 0x80990088: return "installed content conflict";
        case 0x80991404: return "package source unavailable; keep the PC server running";
        case 0x80991401: return "package source rejected authorization (HTTP 401)";
        case 0x80f00633: return "NP environment rejected registration";
        default: return "PS4 installer API failed";
    }
}
static void save_job(unsigned index, const Job *j) { rx_lock(&job_lock); jobs[index]=*j; rx_unlock(&job_lock); }
static int find_job(const char *id) { for (unsigned i=0;i<MAX_JOBS;i++) if (jobs[i].used&&!strcmp(jobs[i].pkg.content_id,id)) return (int)i; return -1; }
static int allocate_job(const char *id) {
    int found=find_job(id); if (found>=0) return found;
    int oldest=-1;
    for (unsigned i=0;i<MAX_JOBS;i++) {
        if (!jobs[i].used) return (int)i;
        if ((jobs[i].phase==JOB_INSTALLED||jobs[i].phase==JOB_FAILED) &&
            (oldest<0||jobs[i].updated<jobs[oldest].updated)) oldest=(int)i;
    }
    return oldest;
}
static void failed(Job *j, int code, const char *error) {
    j->phase=JOB_FAILED; j->error=code?code:-1; j->updated=rx_now(); snprintf(j->error_text,sizeof(j->error_text),"%s",error);
    if (j->local) transfer_finish(j->path,false);
    log_line("install failed cid=%s task=%d code=0x%08x %s",j->pkg.content_id,j->task,(unsigned)j->error,error);
    notify_event(j->pkg.content_id,j->pkg.title_id,j->title,NOTICE_FAILED,error);
}
static void unconfirmed(Job *j, int api_code, const char *detail) {
    if (j->phase!=JOB_UNCONFIRMED||j->error!=api_code||strcmp(j->error_text,detail))
        log_line("install unconfirmed cid=%s task=%d api=0x%08x %s",j->pkg.content_id,j->task,(unsigned)api_code,detail);
    j->phase=JOB_UNCONFIRMED; j->error=api_code;
    snprintf(j->error_text,sizeof(j->error_text),"%s",detail);
    /* Keep ownership and source data: a polling deadline is not an install error. */
}
static void forget_record(const Job *j) {
    if (j->local) return;
    TaskRecord record;
    if (task_record_load(j->pkg.content_id,j->pkg.kind,&record)) return;
    bool known=j->request.header_sha256[0]||j->request.digest[0];
    bool own_live=j->task>=0&&record.task_id==j->task&&record.size==j->pkg.size;
    if ((!known&&own_live)||task_record_matches(&record,j->task>=0?j->task:record.task_id,&j->request)) {
        int rc=task_record_remove(j->pkg.content_id,j->pkg.kind,record.task_id);
        log_line("task ownership cleanup cid=%s task=%d rc=%d",j->pkg.content_id,record.task_id,rc);
    } else log_line("task ownership retained: unmatched record cid=%s",j->pkg.content_id);
}
static void installed(Job *j) {
    j->phase=JOB_INSTALLED; j->progress=100; j->downloaded=j->total; j->error=0; j->error_text[0]=0; j->updated=rx_now();
    if (j->local) transfer_finish(j->path,true);
    forget_record(j);
    log_line("installed proof cid=%s kind=%d task=%d size=%llu",j->pkg.content_id,j->pkg.kind,j->task,(unsigned long long)j->pkg.size);
    notify_event(j->pkg.content_id,j->pkg.title_id,j->title,NOTICE_INSTALLED,"");
}
static bool read_installed(Job *j, PkgInfo *p, uint64_t *stamp, bool identity) {
    char path[192], local_hash[65]; const char *hash=NULL, *digest=NULL;
    if (identity) {
        if (j->local||j->has_header) { sha256_hex(j->pkg.header,sizeof(j->pkg.header),local_hash); hash=local_hash; }
        if (!j->local) {
            if (j->request.header_sha256[0]) hash=j->request.header_sha256;
            digest=j->request.digest;
        }
    }
    if (pkg_find_installed(&j->pkg,hash,digest,p,path,sizeof(path),stamp)) return false;
    if (strcmp(j->proof_path,path)) log_line("installed container cid=%s path=%s",j->pkg.content_id,path);
    snprintf(j->proof_path,sizeof(j->proof_path),"%s",path); return true;
}
static bool installed_task_finished(const Job *j) {
    int task=-1, rc=api.find(j->pkg.content_id,(int)j->pkg.kind,&task);
    log_line("already installed BGFT check cid=%s rc=0x%08x task=%d",j->pkg.content_id,(unsigned)rc,task);
    if (rc && (uint32_t)rc!=0x80990019u) return false;
    if (!rc && task>=0) {
        GsBgftProgress p={0}; rc=api.progress(task,&p);
        uint64_t done=p.length_total?p.transferred_total:p.transferred, total=p.length_total?p.length_total:p.length;
        if (rc||p.error||!total||done<total) return false;
    }
    if (j->pkg.kind!=PKG_DLC) {
        int exists=0; rc=api.app_exists(j->pkg.title_id,&exists);
        if (rc||!exists) return false;
    }
    return true;
}
int install_inventory_title_ready(const PkgInfo *package) {
    if (!package || package->kind != PKG_BASE || !valid_title_id(package->title_id)) return 0;
    for (unsigned i = 0; i < MAX_JOBS; ++i) {
        const Job *job = &jobs[i];
        if (job->used && !strcmp(job->pkg.title_id, package->title_id) &&
            job->phase != JOB_INSTALLED && job->phase != JOB_FAILED) return 0;
    }
    Job candidate = {0};
    candidate.pkg = *package;
    return installed_task_finished(&candidate) ? 1 : 0;
}
static bool proof(Job *j, bool progress_complete) {
    PkgInfo p; uint64_t stamp;
    if (!read_installed(j,&p,&stamp,true)) { j->proof_count=0; return false; }
    if (j->has_header) {
        if (memcmp(p.header,j->pkg.header,4096)) return false;
        if (!j->local||j->phase==JOB_INSTALLED) return true;
        uint64_t now=rx_now(); if (j->proof_checked && now-j->proof_checked<10000) return false;
        j->proof_checked=now;
        int source=rx_open(j->path,RX_READ); char matched_path[192];
        int rc=source>=0?pkg_verify_installed_copy(source,&j->pkg,matched_path,sizeof(matched_path)):-1;
        if (source>=0&&rx_close(source)) rc=-1;
        if (!rc) snprintf(j->proof_path,sizeof(j->proof_path),"%s",matched_path);
        log_line("DLC full-file proof cid=%s result=%d path=%s",j->pkg.content_id,rc,j->proof_path);
        return rc==0;
    }
    /* Missing identity retains the conservative legacy proof. Known identity was
       checked above; active downloads still require BGFT completion and stable disk evidence. */
    bool known_identity=j->request.header_sha256[0]||j->request.digest[0];
    bool changed=!j->baseline_exists||stamp!=j->baseline_stamp||strcmp(j->proof_path,j->baseline_path)||memcmp(p.header+0xfe0,j->baseline_digest,32);
    if (!progress_complete||(!known_identity&&!changed)) { j->proof_count=0; return false; }
    if (j->proof_count && j->proof_stamp==stamp) { memcpy(j->pkg.header,p.header,4096); j->has_header=true; return true; }
    j->proof_stamp=stamp; j->proof_count=1; return false;
}
static int start_task(Job *j, bool resume) {
    int rc=-1;
    if (resume) { rc=api.resume(j->task); log_line("BGFT resume task=%d rc=0x%08x",j->task,(unsigned)rc); if (!rc) return 0; }
    if (api.start) { rc=api.start(j->task); log_line("BGFT start task=%d rc=0x%08x",j->task,(unsigned)rc); }
    if (rc && api.int_start) { rc=api.int_start(j->task); log_line("BGFT internal start task=%d rc=0x%08x",j->task,(unsigned)rc); }
    if (rc) {
        GsBgftProgress prior={0}; bool have_prior=false;
        for (unsigned attempt=0;attempt<=10;attempt++) {
            if (attempt) rx_sleep(500);
            GsBgftProgress current={0}; int probe=api.progress(j->task,&current);
            if (probe) { log_line("BGFT start progress task=%d rc=0x%08x",j->task,(unsigned)probe); continue; }
            if (current.error) return current.error;
            uint64_t done=current.length_total?current.transferred_total:current.transferred;
            uint64_t total=current.length_total?current.length_total:current.length;
            uint64_t before=prior.length_total?prior.transferred_total:prior.transferred;
            if ((total&&done>=total) || (have_prior&&(done>before||current.copy>prior.copy||current.preparing>prior.preparing))) {
                log_line("BGFT start acknowledged by progress task=%d",j->task); return 0;
            }
            prior=current; have_prior=true;
        }
    }
    return rc;
}
static bool owned_task(int task, const UrlRequest *request) {
    TaskRecord record;
    bool owned=!task_record_load(request->content_id,request->kind,&record)&&task_record_matches(&record,task,request);
    log_line("BGFT ownership cid=%s subtype=%d task=%d matched=%d",request->content_id,request->kind,task,owned);
    return owned;
}
static int save_ownership(const Job *j) {
    TaskRecord record; memset(&record,0,sizeof(record)); record.task_id=j->task;
    snprintf(record.content_id,sizeof(record.content_id),"%s",j->pkg.content_id); record.kind=j->pkg.kind; record.size=j->pkg.size;
    record.has_declared_size=j->request.has_declared_size; record.declared_size=j->request.declared_size;
    snprintf(record.header_sha256,sizeof(record.header_sha256),"%s",j->request.header_sha256);
    snprintf(record.digest,sizeof(record.digest),"%s",j->request.digest); record.created=rx_wall_time();
    int rc=task_record_save(&record); log_line("BGFT ownership save cid=%s task=%d rc=%d",j->pkg.content_id,j->task,rc); return rc;
}
static bool same_request(const UrlRequest *a, const UrlRequest *b) {
    if (a->size!=b->size||a->kind!=b->kind||strcmp(a->content_id,b->content_id)) return false;
    if (a->digest[0]&&b->digest[0]&&strcmp(a->digest,b->digest)) return false;
    if (a->has_declared_size&&b->has_declared_size&&a->declared_size!=b->declared_size) return false;
    if (a->header_sha256[0]||b->header_sha256[0]) return a->header_sha256[0]&&b->header_sha256[0]&&!strcmp(a->header_sha256,b->header_sha256);
    if (a->digest[0]||b->digest[0]) return a->digest[0]&&b->digest[0]&&!strcmp(a->digest,b->digest);
    return !strcmp(a->url,b->url);
}
static void submit(Command *c) {
    bool local=c->command==CMD_INSTALL_PKG||c->command==CMD_INSTALL_THEME;
    bool theme=c->command==CMD_INSTALL_THEME||(c->command==CMD_INSTALL_URL&&c->url.theme); PkgInfo p; memset(&p,0,sizeof(p));
    int rc=0, index=-1; char error[256]="";
    if (init_code) { rc=init_code; snprintf(error,sizeof(error),"installer initialization failed; inspect GET_CONFIG"); goto result; }
    if (local) {
        if (transfer_pin(c->path,&p)) { rc=-1; snprintf(error,sizeof(error),"path must be a verified, idle CNT upload"); goto result; }
        if (p.kind!=PKG_DLC) { transfer_finish(c->path,false); rc=-1; snprintf(error,sizeof(error),"local installation accepts DLC only; use INSTALL_URL for games and updates"); goto result; }
        /* Themes are additional content with IRO tag 2; their license-only
           unlocker is AL content. Neither belongs to an installed game. */
        if (theme && !(p.content_type==0x1b&&p.iro_tag==2) && p.content_type!=0x1c) { transfer_finish(c->path,false); rc=-1; snprintf(error,sizeof(error),"theme installation accepts system themes and their license packages only"); goto result; }
    } else {
        snprintf(p.content_id,sizeof(p.content_id),"%s",c->url.content_id); snprintf(p.title_id,sizeof(p.title_id),"%s",c->url.title_id);
        p.kind=c->url.kind; p.size=c->url.size;
        if (theme && (!c->url.has_content_type||c->url.content_type==0x1b)) { p.content_type=0x1b; p.iro_tag=2; }
    }
    index=allocate_job(p.content_id);
    if (index<0) { if (local) transfer_finish(c->path,false); rc=-1; snprintf(error,sizeof(error),"install job capacity busy"); goto result; }
    Job j; memset(&j,0,sizeof(j));
    if (jobs[index].used && !strcmp(jobs[index].pkg.content_id,p.content_id) && jobs[index].pkg.kind==p.kind &&
        jobs[index].pkg.size==p.size && (local?jobs[index].has_header&&!memcmp(jobs[index].pkg.header,p.header,4096):same_request(&jobs[index].request,&c->url))) {
        j=jobs[index];
        if (j.phase==JOB_INSTALLED && proof(&j,true)) { if (local) transfer_finish(c->path,true); goto accepted; }
        if (!local&&j.phase!=JOB_FAILED&&j.phase!=JOB_INSTALLED&&j.total&&j.downloaded>=j.total&&proof(&j,true)) {
            installed(&j); save_job((unsigned)index,&j); goto accepted;
        }
        if (j.phase!=JOB_FAILED && j.phase!=JOB_INSTALLED) {
            if (local) transfer_finish(c->path,false);
            else {
                if (!owned_task(j.task,&c->url)) { rc=-EEXIST; snprintf(error,sizeof(error),"%s",TASK_CONFLICT); goto result; }
                j.request=c->url;
                rc=start_task(&j,true); if (rc) goto job_error;
                j.paused=false; j.phase=JOB_QUEUED; j.movement=j.updated=rx_now(); save_job((unsigned)index,&j);
            }
            goto accepted;
        }
    } else if (jobs[index].used && jobs[index].phase!=JOB_FAILED && jobs[index].phase!=JOB_INSTALLED) {
        if (local) transfer_finish(c->path,false); rc=-1; snprintf(error,sizeof(error),"%s",local?"another install with this content ID is active":TASK_CONFLICT); goto result;
    }
    memset(&j,0,sizeof(j)); j.used=true; j.local=local; j.has_header=local; j.task=-1; j.pkg=p; j.request=c->url;
    j.total=p.size; j.created=j.updated=j.movement=rx_now(); j.phase=JOB_QUEUED;
    if (local) {
        TitleContext title; notify_get_context(&title); snprintf(j.path,sizeof(j.path),"%s",c->path);
        snprintf(j.title,sizeof(j.title),"%s",!strcmp(title.title_id,p.title_id)&&title.title[0]?title.title:p.title_id);
    } else snprintf(j.title,sizeof(j.title),"%s",c->url.title);
    PkgInfo prior; uint64_t stamp;
    if (read_installed(&j,&prior,&stamp,false)) {
        j.baseline_exists=true; j.baseline_stamp=stamp; memcpy(j.baseline_digest,prior.header+0xfe0,32);
        snprintf(j.baseline_path,sizeof(j.baseline_path),"%s",j.proof_path);
    }
    save_job((unsigned)index,&j);
    if (local && proof(&j,false)) { installed(&j); save_job((unsigned)index,&j); goto accepted; }
    /* Final-size containers can exist while BGFT is still downloading. Exact
       identity alone does not override an incomplete or failed active task. */
    if (!local && (j.request.header_sha256[0]||j.request.digest[0]) && read_installed(&j,&prior,&stamp,true) && installed_task_finished(&j)) {
        memcpy(j.pkg.header,prior.header,4096); j.has_header=true;
        log_line("already installed cid=%s path=%s",j.pkg.content_id,j.proof_path);
        installed(&j); save_job((unsigned)index,&j); goto accepted;
    }
    if (p.kind!=PKG_BASE && !theme) {
        int exists=0; rc=api.app_exists(p.title_id,&exists); log_line("base exists %s rc=0x%08x exists=%d",p.title_id,(unsigned)rc,exists);
        if (!rc&&!exists) rc=(int)0x80a30004u;
        if (rc) goto job_error;
    }
    if (local) {
        rc=api.app_pkg(j.path,NULL); log_line("AppInstallPkg cid=%s type=0x%02x iro=%u rc=0x%08x",p.content_id,(unsigned)p.content_type,(unsigned)p.iro_tag,(unsigned)rc);
        if (rc) goto job_error;
        j.phase=JOB_INSTALLING;
    } else {
        int task=-1; rc=api.find(p.content_id,(int)p.kind,&task);
        log_line("BGFT find cid=%s subtype=%d rc=0x%08x task=%d",p.content_id,p.kind,(unsigned)rc,task);
        if (!rc&&task>=0) {
            if (!owned_task(task,&j.request)) { rc=-EEXIST; snprintf(error,sizeof(error),"%s",TASK_CONFLICT); goto job_error; }
            j.task=task; save_job((unsigned)index,&j); rc=start_task(&j,true);
        } else {
            /* A failed lookup other than NOT_FOUND is ambiguous ownership. */
            if (rc && (uint32_t)rc!=0x80990019u) goto job_error;
            if (rx_mkdir(TASK_ROOT)) { rc=-EIO; snprintf(error,sizeof(error),"task ownership directory is not writable"); goto job_error; }
            GsBgftParam param; memset(&param,0,sizeof(param));
            param.user=user_id; param.entitlement=5; param.id=j.pkg.content_id; param.url=j.request.url;
            param.name=j.title; param.icon=j.request.icon_url; param.option=0x10000; param.scenario="0";
            param.type=j.request.has_content_type?pkg_bgft_type(j.request.content_type):p.kind==PKG_DLC?"PS4AC":"PS4GD";
            param.subtype=""; param.size=j.request.has_declared_size?j.request.declared_size:p.size;
            rc=(p.kind==PKG_BASE?api.reg:api.debug_reg)(&param,&j.task);
            log_line("BGFT register cid=%s subtype=%d rc=0x%08x task=%d user=%d size=%llu declared=%llu type=%s",p.content_id,p.kind,(unsigned)rc,j.task,user_id,(unsigned long long)p.size,(unsigned long long)param.size,param.type);
            if (j.task>=0 && save_ownership(&j)) { rc=-EIO; snprintf(error,sizeof(error),"BGFT task exists but its ownership record could not be saved; ownership unconfirmed"); goto job_error; }
            if (rc||j.task<0) { if (!rc) { rc=-1; snprintf(error,sizeof(error),"BGFT accepted registration without a task ID; ownership unconfirmed"); } goto job_error; }
            save_job((unsigned)index,&j); rc=start_task(&j,false);
        }
        if (rc) goto job_error;
    }
    j.updated=j.movement=rx_now(); save_job((unsigned)index,&j);
    notify_event(p.content_id,p.title_id,j.title,NOTICE_INSTALLING,"");
accepted:
    c->code=RESP_OK;
    if (submission_json(c->result,sizeof(c->result),0,p.content_id,local?c->path:c->url.url,"",j.task,!local)) c->code=RESP_ERROR;
    return;
job_error:
    if (!error[0]) snprintf(error,sizeof(error),"%s (0x%08x)",install_error(rc),(unsigned)rc);
    failed(&j,rc,error); save_job((unsigned)index,&j);
result:
    c->code=RESP_ERROR;
    if (submission_json(c->result,sizeof(c->result),rc?rc:-1,p.content_id,local?c->path:c->url.url,error,index>=0?jobs[index].task:-1,!local)) snprintf(c->result,sizeof(c->result),"JSON reply overflow");
}
static void control(Command *c) {
    int index=find_job(c->cid); c->code=RESP_ERROR;
    if (index<0) { snprintf(c->result,sizeof(c->result),"unknown install"); return; }
    Job j=jobs[index];
    if (j.local) { snprintf(c->result,sizeof(c->result),"not cancellable"); return; }
    if (j.task<0||j.phase==JOB_INSTALLED) { snprintf(c->result,sizeof(c->result),"no active BGFT task"); return; }
    int rc;
    if (c->command==CMD_CANCEL_INSTALL) {
        rc=api.stop(j.task); log_line("BGFT stop task=%d rc=0x%08x",j.task,(unsigned)rc);
        if (!rc) { rc=api.unregister_task(j.task); log_line("BGFT unregister task=%d rc=0x%08x",j.task,(unsigned)rc); }
        if (!rc) { forget_record(&j); failed(&j,-ECANCELED,"install cancelled"); j.task=-1; }
    } else {
        bool pause=c->command==CMD_PAUSE_INSTALL;
        rc=(pause?api.pause:api.resume)(j.task); log_line("BGFT %s task=%d rc=0x%08x",pause?"pause":"resume",j.task,(unsigned)rc);
        if (!rc) { j.paused=pause; j.phase=pause?JOB_PAUSED:JOB_QUEUED; j.error=0; j.error_text[0]=0; j.movement=rx_now(); }
    }
    if (rc) snprintf(c->result,sizeof(c->result),"%s (0x%08x)",install_error(rc),(unsigned)rc);
    else { save_job((unsigned)index,&j); c->code=RESP_OK; snprintf(c->result,sizeof(c->result),"OK"); }
}
static void theme_active(char *out, size_t cap) {
    out[0]=0;
    if (!api.theme_get||user_id<0) return;
    char raw[128]; memset(raw,0,sizeof(raw));
    int rc=api.theme_get(user_id,raw,64);
    char hex[97]; for (unsigned i=0;i<48;i++) snprintf(hex+2*i,3,"%02x",(unsigned char)raw[i]);
    log_line("theme entitlement get user=%d rc=0x%08x raw=%s",user_id,(unsigned)rc,hex);
    if (rc) return;
    size_t n=0; while (n<64 && n+1<cap && raw[n] && ((raw[n]>='A'&&raw[n]<='Z')||(raw[n]>='0'&&raw[n]<='9')||raw[n]=='-'||raw[n]=='_')) { out[n]=raw[n]; n++; }
    out[n]=0;
}
static void theme_command(Command *c) {
    c->code=RESP_ERROR;
    if (c->command==CMD_THEME_LIST) {
        char active[65]; theme_active(active,sizeof(active));
        if (installed_library_themes_json(c->result,sizeof(c->result),active)) snprintf(c->result,sizeof(c->result),"installed-theme scan failed");
        else c->code=RESP_DATA;
        return;
    }
    if (!installed_library_theme_present(c->cid)) { snprintf(c->result,sizeof(c->result),"that theme is not installed on this PS4"); return; }
    int rc;
    if (c->command==CMD_THEME_DELETE) {
        if (!api.theme_uninstall) { snprintf(c->result,sizeof(c->result),"this firmware does not offer theme removal"); return; }
        rc=api.theme_uninstall(c->cid); log_line("theme uninstall cid=%s rc=0x%08x",c->cid,(unsigned)rc);
    } else {
        if (!api.theme_set||user_id<0) { snprintf(c->result,sizeof(c->result),"this firmware does not offer theme selection"); return; }
        /* The entitlement is the 16-character label after the content ID's last dash. */
        char label[64]; memset(label,0,sizeof(label)); memcpy(label,c->cid+20,16);
        rc=api.theme_set(user_id,label); log_line("theme entitlement set user=%d label=%s rc=0x%08x",user_id,label,(unsigned)rc);
    }
    if (rc) snprintf(c->result,sizeof(c->result),"PS4 refused the theme request (0x%08x)",(unsigned)rc);
    else { c->code=RESP_OK; snprintf(c->result,sizeof(c->result),"OK"); }
}
static void poll_job(unsigned index) {
    Job j=jobs[index]; if (!j.used||j.phase==JOB_INSTALLED||j.phase==JOB_FAILED) return;
    uint64_t now=rx_now(); bool complete=false;
    if (!j.local) {
        GsBgftProgress p; memset(&p,0,sizeof(p)); int rc=api.progress(j.task,&p);
        if (rc) {
            bool was_complete=j.total&&j.downloaded>=j.total;
            if (proof(&j,was_complete)) installed(&j);
            else unconfirmed(&j,rc,"PS4 install progress is unavailable; installed content is not yet confirmed");
            goto report;
        }
        if (p.error) { failed(&j,p.error,install_error(p.error)); save_job(index,&j); return; }
        bool was_complete=j.total&&j.downloaded>=j.total;
        j.error=0; j.error_text[0]=0;
        uint64_t done=p.length_total?p.transferred_total:p.transferred, total=p.length_total?p.length_total:p.length;
        if (done!=j.downloaded||p.preparing!=j.last.preparing||p.copy!=j.last.copy) { j.movement=now; j.moved=true; }
        j.downloaded=done; if (total) j.total=total;
        complete=total&&done>=total;
        if (complete&&!was_complete)
            diagnostic("BGFT transfer complete cid=%s task=%d bytes=%llu/%llu prepare=%d local_copy=%d bits=0x%08x",j.pkg.content_id,j.task,(unsigned long long)done,(unsigned long long)total,p.preparing,p.copy,p.bits);
        j.progress=total?(done>=total?99:(int)((double)done*100.0/(double)total)):0;
        if (j.progress>99) j.progress=99;
        if (j.paused) j.phase=JOB_PAUSED;
        else if (complete||p.copy>0||p.preparing>0) { j.phase=JOB_INSTALLING; if (p.copy>0&&p.copy<100) j.progress=p.copy; }
        else j.phase=done?JOB_DOWNLOADING:JOB_QUEUED;
        /* localCopyPercent describes a separate storage-copy stage. HTTP tasks
           can leave it at zero after completion; matching, stable installed
           package metadata remains mandatory in addition to completed bytes. */
        if (proof(&j,complete)) installed(&j);
        j.last=p;
    } else if (proof(&j,false)) installed(&j);
    if (j.phase!=JOB_INSTALLED&&!j.paused && now-j.movement>=(complete?180000u:600000u))
        unconfirmed(&j,0,complete?"download complete; waiting for PS4 installed-content confirmation":"PS4 installer has not reported progress; installation remains unconfirmed");
report:
    if (j.phase!=jobs[index].phase || now-j.updated>=5000) {
        log_line("install status cid=%s task=%d phase=%d bytes=%llu/%llu prepare=%d copy=%d",j.pkg.content_id,j.task,j.phase,(unsigned long long)j.downloaded,(unsigned long long)j.total,j.last.preparing,j.last.copy);
        j.updated=now;
    }
    save_job(index,&j);
}
bool install_busy(void) {
    rx_lock(&command_lock); bool busy=pending.busy; rx_unlock(&command_lock);
    rx_lock(&job_lock);
    for (unsigned i=0;i<MAX_JOBS;i++) if (jobs[i].used && jobs[i].phase!=JOB_INSTALLED && jobs[i].phase!=JOB_FAILED) busy=true;
    rx_unlock(&job_lock); return busy;
}
void install_worker_tick(void) {
    rx_lock(&command_lock); bool execute=pending.busy&&!pending.done; Command work;
    if (execute) work=pending; rx_unlock(&command_lock);
    if (execute) {
        if (work.command==CMD_INSTALL_URL||work.command==CMD_INSTALL_PKG||work.command==CMD_INSTALL_THEME) submit(&work);
        else if (work.command==CMD_THEME_LIST||work.command==CMD_THEME_APPLY||work.command==CMD_THEME_DELETE) theme_command(&work);
        else if (work.command==CMD_LIST_INSTALLED) {
            work.code = installed_library_list_json(work.result, sizeof(work.result)) ? RESP_ERROR : RESP_DATA;
            if (work.code == RESP_ERROR) snprintf(work.result, sizeof(work.result), "installed-library scan failed");
        } else control(&work);
        rx_lock(&command_lock);
        if (pending.abandoned) memset(&pending,0,sizeof(pending));
        else { memcpy(pending.result,work.result,sizeof(pending.result)); pending.result[sizeof(pending.result)-1]=0; pending.code=work.code; pending.done=true; }
        rx_unlock(&command_lock);
    }
    uint64_t now=rx_now(); if (init_code||now<next_poll) return; next_poll=now+1000;
    for (unsigned i=0;i<MAX_JOBS;i++) poll_job(i);
}
static int command_error(int fd, const Command *c, int code, const char *error) {
    if (c->command!=CMD_INSTALL_URL&&c->command!=CMD_INSTALL_PKG&&c->command!=CMD_INSTALL_THEME) return text_reply(fd,RESP_ERROR,error);
    char text[16384]; bool url=c->command==CMD_INSTALL_URL;
    if (submission_json(text,sizeof(text),code,url?c->url.content_id:"",url?c->url.url:c->path,error,-1,url)) return -1;
    return text_reply(fd,RESP_ERROR,text);
}
int install_request(int fd, unsigned cmd, const uint8_t *body, size_t size) {
    if (cmd==CMD_INSTALL_URL||cmd==CMD_INSTALL_PKG||cmd==CMD_INSTALL_THEME) {
        char denied[1024]; int blocked=privilege_error_json(denied,sizeof(denied),privilege_ready,privilege_boot,privilege_jbc);
        if (blocked) return blocked<0?-1:text_reply(fd,RESP_ERROR,denied);
    }
    Command c; memset(&c,0,sizeof(c)); c.command=cmd; char error[256]="";
    if (cmd==CMD_LIST_INSTALLED||cmd==CMD_THEME_LIST) {
        if (size) return command_error(fd,&c,-EINVAL,"this request must be empty");
    } else if (cmd==CMD_INSTALL_URL) {
        if (parse_install_url(body,size,&c.url,error,sizeof(error))) {
            char response[1024]; if (submission_json(response,sizeof(response),-1,"","",error,-1,true)) return -1;
            return text_reply(fd,RESP_ERROR,response);
        }
    } else {
        bool file=cmd==CMD_INSTALL_PKG||cmd==CMD_INSTALL_THEME;
        const char *s=wire_string(body,size,file?MAX_PATH_BYTES:36);
        if (!s || (file?!allowed_path(s):!valid_content_id(s))) return command_error(fd,&c,-EINVAL,"invalid install path or content ID");
        if (file) snprintf(c.path,sizeof(c.path),"%s",s); else snprintf(c.cid,sizeof(c.cid),"%s",s);
    }
    rx_lock(&command_lock);
    if (pending.busy) { rx_unlock(&command_lock); return command_error(fd,&c,-EBUSY,"installer busy or previous operation unconfirmed; query INSTALL_STATUS"); }
    pending=c; pending.busy=true; rx_unlock(&command_lock);
    uint64_t start=rx_now();
    for (;;) {
        rx_lock(&command_lock);
        if (pending.done) { c=pending; memset(&pending,0,sizeof(pending)); rx_unlock(&command_lock); return text_reply(fd,c.code,c.result); }
        if (rx_now()-start>=30000) { pending.abandoned=true; rx_unlock(&command_lock); diagnostic("installer command 0x%02x timed out; ownership unconfirmed",cmd); return command_error(fd,&c,-ETIMEDOUT,"installer operation timed out; ownership unconfirmed; query INSTALL_STATUS before retrying"); }
        rx_unlock(&command_lock); rx_sleep(20);
    }
}
int install_status_reply(int fd, const char *cid) {
    rx_lock(&job_lock); int index=find_job(cid); Job j; if (index>=0) j=jobs[index]; rx_unlock(&job_lock);
    if (index<0) return text_reply(fd,RESP_ERROR,"unknown install");
    uint64_t timeout=j.total&&j.downloaded>=j.total?180000u:600000u;
    if (j.phase!=JOB_INSTALLED && j.phase!=JOB_FAILED && !j.paused && rx_now()-j.updated>timeout) {
        j.phase=JOB_UNCONFIRMED; j.error=0; snprintf(j.error_text,sizeof(j.error_text),"installer worker is unresponsive; installation remains unconfirmed");
    }
    const char *phases[]={"queued","downloading","paused","installing","installed","failed","awaiting_confirmation"};
    InstallStatus s={j.error,j.phase==JOB_FAILED?j.error:0,j.progress,j.pkg.content_id,j.phase==JOB_INSTALLED?"complete":j.phase==JOB_FAILED?"failed":j.phase==JOB_UNCONFIRMED?"unconfirmed":"installing",phases[j.phase],j.error_text,j.downloaded,j.total};
    char text[2048]; if (status_json(text,sizeof(text),&s)) return -1;
    return text_reply(fd,j.phase==JOB_FAILED?RESP_ERROR:RESP_DATA,text);
}
int install_preflight(int fd, bool writable) {
    char denied[1024]; int blocked=privilege_error_json(denied,sizeof(denied),privilege_ready,privilege_boot,privilege_jbc);
    if (blocked) return blocked<0?-1:text_reply(fd,RESP_ERROR,denied);
    int rc=init_code; const char *stage="installer initialization", *error="inspect GET_CONFIG module diagnostics";
    struct statfs fs; uint64_t free_bytes=0;
    if (!writable) { rc=-EACCES; stage="data root"; error="/user/data/sspi-receiver is not writable"; }
    if (!rc) {
        if (statfs("/user",&fs)) { rc=-errno; stage="statfs /user"; error="could not query PS4 free space"; }
        else if (fs.f_bavail>0 && fs.f_bsize && (uint64_t)fs.f_bavail<=UINT64_MAX/(uint64_t)fs.f_bsize)
            free_bytes=(uint64_t)fs.f_bavail*(uint64_t)fs.f_bsize;
    }
    char text[1024];
    if (rc) { if (preflight_error_json(text,sizeof(text),rc,stage,error)) return -1; }
    else snprintf(text,sizeof(text),"OK {\"bgft\":\"ready\",\"appinst\":\"ready\",\"free\":%llu}",(unsigned long long)free_bytes);
    return text_reply(fd,rc?RESP_ERROR:RESP_OK,text);
}
