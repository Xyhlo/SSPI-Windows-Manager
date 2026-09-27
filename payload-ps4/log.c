#include "log.h"
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#ifdef SSPI_BINLOADER
#include "runtime.h"
#define LOG_FALLBACK(...) payload_debug(__VA_ARGS__)
#else
#define LOG_FALLBACK(...) fprintf(stderr,__VA_ARGS__)
#endif
static RxMutex log_lock=RX_MUTEX_INIT, diag_lock=RX_MUTEX_INIT;
static char messages[128][256];
static unsigned count;
void log_line(const char *format, ...) {
    char detail[768], line[832]; va_list a; va_start(a,format); int n=vsnprintf(detail,sizeof(detail),format,a); va_end(a);
    if (n<0) return;
    n=snprintf(line,sizeof(line),"[%llu] %s\n",(unsigned long long)rx_now(),detail); if (n<0) return;
    size_t len=(size_t)n<sizeof(line)?(size_t)n:sizeof(line)-1;
    rx_lock(&log_lock); uint64_t size=0, stamp=0;
    if (!rx_stat(DATA_ROOT "/receiver.log",&size,&stamp) && size+len>1024u*1024u) {
        if (rx_rename(DATA_ROOT "/receiver.log",DATA_ROOT "/receiver.log.1")) { rx_unlock(&log_lock); return; } size=0;
    }
    int fd=rx_open(DATA_ROOT "/receiver.log",RX_APPEND);
    if (fd>=0) {
        int rc=rx_write_exact(fd,line,len,size); if (!rc) rc=rx_sync(fd);
        int closed=rx_close(fd); if (rc||closed) LOG_FALLBACK("SSPI log write failed\n");
    } else LOG_FALLBACK("%s",line);
    rx_unlock(&log_lock);
}
void diagnostic(const char *format, ...) {
    char text[256]; va_list a; va_start(a,format); int n=vsnprintf(text,sizeof(text),format,a); va_end(a); if (n<0) return;
    rx_lock(&diag_lock);
    if (count<128) { snprintf(messages[count],sizeof(messages[count]),"%s",text); count++; }
    rx_unlock(&diag_lock); log_line("%s",text);
}
void diagnostics_json(Json *j) {
    rx_lock(&diag_lock); json_add(j,"[");
    for (unsigned i=0;i<count;i++) { if (i) json_add(j,","); json_quote(j,messages[i]); }
    json_add(j,"]"); rx_unlock(&diag_lock);
}
