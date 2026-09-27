#include "proto.h"
#include <string.h>
#include <stdio.h>

int send_all(int fd, const void *data, size_t size) {
    const uint8_t *p=data;
    while (size) { int64_t n=rx_send(fd,p,size); if (n<=0 || (uint64_t)n>size) return -1; p+=n; size-=(size_t)n; }
    return 0;
}
void receive_begin(ReceiveDeadline *d, size_t size) { *d=(ReceiveDeadline){rx_now(),0,size>RECEIVE_MIN_BYTES}; }
int recv_all_deadline(int fd, void *data, size_t size, ReceiveDeadline *d) {
    uint8_t *p=data;
    while (size) {
        uint64_t now=rx_now(); if (now-d->started>=RECEIVE_WINDOW_MS) return -1;
        int64_t n=rx_recv(fd,p,size,d->started+RECEIVE_WINDOW_MS);
        now=rx_now(); if (n<=0||(uint64_t)n>size||now-d->started>=RECEIVE_WINDOW_MS) return -1;
        p+=n; size-=(size_t)n; d->received+=(size_t)n;
        if (d->streaming && d->received>=RECEIVE_MIN_BYTES) { d->started=now; d->received=0; }
    }
    return 0;
}
int recv_all(int fd, void *data, size_t size) { ReceiveDeadline d; receive_begin(&d,size); return recv_all_deadline(fd,data,size,&d); }
int read_frame_deadline(int fd, uint8_t *cmd, uint32_t *size, ReceiveDeadline *d) {
    receive_begin(d,5); uint8_t h[5]; if (recv_all_deadline(fd,h,sizeof(h),d)) return -1;
    *cmd=h[0]; *size=read_u32le(h+1); if (*size>MAX_FRAME) return -2;
    if (*size>RECEIVE_MIN_BYTES) receive_begin(d,*size);
    return 0;
}
int read_frame(int fd, uint8_t *cmd, uint32_t *size) { ReceiveDeadline d; return read_frame_deadline(fd,cmd,size,&d); }
int reply(int fd, uint8_t code, const void *data, uint32_t size) {
    if (size>MAX_FRAME) return -1;
    uint8_t h[5]; h[0]=code; write_u32le(h+1,size);
    return send_all(fd,h,5) || (size && send_all(fd,data,size)) ? -1 : 0;
}
int text_reply(int fd, uint8_t code, const char *text) { return reply(fd,code,text,(uint32_t)strlen(text)); }
const char *wire_string(const uint8_t *b, size_t n, size_t limit) {
    if (!b || n<2 || n>limit+1 || b[n-1] || memchr(b,0,n-1)) return NULL;
    return (const char *)b;
}
int parse_port(const char *p, size_t n, bool config) {
    if (!p || !n) return -1;
    if (config) { if (n<6 || memcmp(p,"port=",5)) return -1; p+=5; n-=5; }
    if (n && p[n-1]=='\n') { n--; if (n && p[n-1]=='\r') n--; }
    if (!n || n>5) return -1;
    unsigned v=0; for (size_t i=0;i<n;i++) { if (p[i]<'0'||p[i]>'9') return -1; v=v*10+(unsigned)(p[i]-'0'); }
    return v>=1024 && v<=65535 ? (int)v : -1;
}
