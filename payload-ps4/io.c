#include "platform.h"
#include <stdio.h>
#include <string.h>

int rx_read_exact(int fd, void *data, size_t n, uint64_t offset) {
    uint8_t *p=data; while (n) { int64_t r=rx_pread(fd,p,n,offset); if (r<=0 || (uint64_t)r>n) return -1; n-=(size_t)r; p+=r; offset+=(uint64_t)r; } return 0;
}
int rx_write_exact(int fd, const void *data, size_t n, uint64_t offset) {
    const uint8_t *p=data; while (n) { int64_t r=rx_pwrite(fd,p,n,offset); if (r<=0 || (uint64_t)r>n) return -1; n-=(size_t)r; p+=r; offset+=(uint64_t)r; } return 0;
}
int rx_atomic_file(const char *path, const void *data, size_t size) {
    char tmp[2200]; int n=snprintf(tmp,sizeof(tmp),"%s.next",path);
    if (n<0 || (size_t)n>=sizeof(tmp)) return -1;
    int fd=rx_open(tmp,RX_CREATE); if (fd<0) return -1;
    int rc=rx_write_exact(fd,data,size,0); if (rx_sync(fd)) rc=-1;
    if (rx_close(fd)) rc=-1;
    if (!rc) rc=rx_rename(tmp,path);
    if (rc && rx_unlink(tmp)) return -1;
    return rc;
}
