#define _GNU_SOURCE
#include "runtime.h"
#include "platform.h"
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <unistd.h>
#include <orbis/libkernel.h>

extern int payload_dlsym(int handle, const char *name, void **symbol);
extern int payload_load_module(const char *name, int flags, int *handle, int unused);
extern int main(void);
#define RX_IMPORT(module, symbol, name) extern void *rx_import_##symbol;
#include "imports.inc"
#undef RX_IMPORT
enum { KERNEL, LIBC };
static const struct { unsigned module; void **slot; const char *name; } imports[] = {
#define RX_IMPORT(module, symbol, name) { module, &rx_import_##symbol, #name },
#include "imports.inc"
#undef RX_IMPORT
};
static int boot_fd=-1;
static int kernel_module=-1, libc_module=-1, kernel_sys_module=-1;
static bool kernel_sys_tried;
static int (*boot_open)(const char *, int, ...);
static int (*boot_close)(int);
static ssize_t (*boot_write)(int, const void *, size_t);
static int (*boot_notify)(int, OrbisNotificationRequest *, size_t, int);

static size_t boot_length(const char *text) {
    size_t size=0; while (text[size]) size++; return size;
}
static void boot_text(const char *text) {
    if (boot_write && boot_fd>=0) (void)boot_write(boot_fd,text,boot_length(text));
}
static void boot_toast(const char *text) {
    if (!boot_notify) return;
    OrbisNotificationRequest request;
    volatile unsigned char *zero=(volatile unsigned char *)&request;
    for (size_t i=0;i<sizeof(request);i++) zero[i]=0;
    request.targetId=-1;
    size_t i=0; while (text[i]&&i+1<sizeof(request.message)) { request.message[i]=text[i]; i++; }
    (void)boot_notify(0,&request,sizeof(request),0);
}
static int boot_failed(const char *detail) {
    boot_text("SSPI bootstrap failed: "); boot_text(detail); boot_text("\n");
    boot_toast("SSPI receiver bootstrap failed; inspect /user/data/sspi-receiver/bootstrap.log");
    if (boot_close && boot_fd>=0) (void)boot_close(boot_fd);
    boot_fd=-1; return 1;
}
void payload_debug(const char *format, ...) {
    char text[1024]; va_list args; va_start(args,format);
    int count=vsnprintf(text,sizeof(text),format,args); va_end(args);
    if (count<0) return;
    int fd=open("/dev/klog",O_WRONLY,0);
    if (fd>=0) { (void)write(fd,text,(size_t)count<sizeof(text)?(size_t)count:sizeof(text)-1); (void)close(fd); }
}
/* Diagnostics call functions that some firmware or process contexts lack; a missing
   one must never fail the bootstrap, so they are looked up on first use. */
void *rx_optional_symbol(const char *name) {
    void *symbol=NULL;
    if (kernel_module>=0 && !payload_dlsym(kernel_module,name,&symbol) && symbol) return symbol;
    if (libc_module>=0 && !payload_dlsym(libc_module,name,&symbol) && symbol) return symbol;
    if (!kernel_sys_tried) { kernel_sys_tried=true; int handle=-1; if (!payload_load_module("libkernel_sys.sprx",0,&handle,0)) kernel_sys_module=handle; }
    if (kernel_sys_module>=0 && !payload_dlsym(kernel_sys_module,name,&symbol) && symbol) return symbol;
    return NULL;
}
/* libkernel exports _fstatfs; statfs is normally supplied by the app's libc. */
int statfs(const char *path, struct statfs *info) {
    int fd=open(path,O_RDONLY,0); if (fd<0) return -1;
    int result=fstatfs(fd,info), saved=errno;
    int closed=close(fd); if (result) errno=saved;
    return result?result:closed;
}
int payload_bootstrap(void) {
    int kernel=-1;
    const char *kernel_names[]={"libkernel.sprx","libkernel_web.sprx","libkernel_sys.sprx"};
    for (unsigned i=0;i<3&&kernel<0;i++) {
        int handle=-1;
        if (!payload_load_module(kernel_names[i],0,&handle,0)) kernel=handle;
    }
    /* These handles cover application and system-hosted payload contexts. */
    if (kernel<0) {
        const int handles[]={1,0x2001};
        for (unsigned i=0;i<2;i++) {
            void *probe=NULL;
            if (!payload_dlsym(handles[i],"sceKernelLoadStartModule",&probe)&&probe) { kernel=handles[i]; break; }
        }
    }
    if (kernel<0) return 1;
    (void)payload_dlsym(kernel,"open",(void **)&boot_open);
    (void)payload_dlsym(kernel,"write",(void **)&boot_write);
    (void)payload_dlsym(kernel,"close",(void **)&boot_close);
    (void)payload_dlsym(kernel,"sceKernelSendNotificationRequest",(void **)&boot_notify);
    int (*boot_mkdir)(const char *, unsigned)=NULL;
    (void)payload_dlsym(kernel,"mkdir",(void **)&boot_mkdir);
    if (boot_mkdir) (void)boot_mkdir(DATA_ROOT,0775);
    if (boot_open) boot_fd=boot_open(DATA_ROOT "/bootstrap.log",O_WRONLY|O_CREAT|O_APPEND,0664);
    boot_text("SSPI BinLoader entry reached; resolving native imports\n");
    int (*load_start)(const char *,size_t,const void *,unsigned,void *,int *)=NULL;
    if (payload_dlsym(kernel,"sceKernelLoadStartModule",(void **)&load_start)||!load_start) return boot_failed("sceKernelLoadStartModule");
    int result=0, libc=load_start("libSceLibcInternal.sprx",0,NULL,0,NULL,&result);
    if (libc<0||result<0) return boot_failed("libSceLibcInternal.sprx");
    kernel_module=kernel; libc_module=libc;
    for (size_t i=0;i<sizeof(imports)/sizeof(imports[0]);i++) {
        const int module=imports[i].module==KERNEL?kernel:libc;
        if (payload_dlsym(module,imports[i].name,imports[i].slot)||!*imports[i].slot) return boot_failed(imports[i].name);
    }
    boot_text("SSPI native imports ready; entering receiver main\n");
    if (boot_close && boot_fd>=0) (void)boot_close(boot_fd);
    boot_fd=-1;
    return main();
}
