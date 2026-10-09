#ifdef SSPI_INSTALL_TEST
#include "install-test-platform.h"
#else
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#endif
#include "install_protocol.h"

uint64_t sspi_install_millis(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) return 0;
    return (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
}
bool sspi_install_directory(const char *directory) {
    if (!directory || strncmp(directory, SSPI_INSTALL_ROOT, sizeof(SSPI_INSTALL_ROOT)-1)) return false;
    const char *suffix = directory + sizeof(SSPI_INSTALL_ROOT)-1;
    if (strlen(suffix) != 6) return false;
    for (; *suffix; ++suffix)
        if (!((*suffix >= 'a' && *suffix <= 'z') || (*suffix >= 'A' && *suffix <= 'Z') || (*suffix >= '0' && *suffix <= '9'))) return false;
    return true;
}
bool sspi_install_package_path(const char *path) {
    static const char prefix[] = "/user/data/tmp/upload_";
    if (!path || strncmp(path, prefix, sizeof(prefix)-1)) return false;
    size_t n = strlen(path);
    if (n <= sizeof(prefix)+3 || n >= SSPI_INSTALL_PATH || strcmp(path+n-4, ".pkg")) return false;
    for (const char *p = path+sizeof(prefix)-1; *p; ++p)
        if (!((*p >= 'a' && *p <= 'z') || (*p >= 'A' && *p <= 'Z') || (*p >= '0' && *p <= '9') || *p == '-' || *p == '_' || *p == '.')) return false;
    return strstr(path, "..") == NULL;
}
bool sspi_install_content_id(const char *id) {
    size_t n = strlen(id);
    if (n < 9 || n >= 48) return false;
    for (; *id; ++id)
        if (!((*id >= 'A' && *id <= 'Z') || (*id >= 'a' && *id <= 'z') || (*id >= '0' && *id <= '9') || *id == '-' || *id == '_')) return false;
    return true;
}
static uint64_t install_u64le(const unsigned char *bytes) {
    uint64_t value=0;
    for (unsigned i=0;i<8;++i) value|=(uint64_t)bytes[i]<<(8*i);
    return value;
}
bool sspi_install_package(int fd,uint64_t size,char content_id[48]) {
    unsigned char header[0x80];
    if (content_id) content_id[0]=0;
    if (size<sizeof(header) || pread(fd,header,sizeof(header),0)!=(ssize_t)sizeof(header)) return false;
    if (!memcmp(header,"\177FIH",4)) {
        uint64_t image=install_u64le(header+0x10), length=install_u64le(header+0x18), cnt=install_u64le(header+0x58);
        if (image<0x10000 || !length || image>size || length>size-image || cnt<image+length || cnt>size || size-cnt<sizeof(header)) return false;
        if (pread(fd,header,sizeof(header),(off_t)cnt)!=(ssize_t)sizeof(header)) return false;
    }
    if (memcmp(header,"\177CNT",4)) return false;
    if (content_id) {
        const unsigned offsets[]={0x40,0x30};
        for (unsigned i=0;i<2;++i) {
            char id[37]={0}; memcpy(id,header+offsets[i],36);
            if (sspi_install_content_id(id)) { snprintf(content_id,48,"%s",id); break; }
        }
    }
    return true;
}
int sspi_install_read(const char *directory, const char *name, void *data, size_t size) {
    char path[128];
    if (!sspi_install_directory(directory) || (strcmp(name,"request") && strcmp(name,"status"))) return -EINVAL;
    snprintf(path, sizeof(path), "%s/%s", directory, name);
    int fd = open(path, O_RDONLY | O_NOFOLLOW);
    if (fd < 0) return -errno;
    struct stat st;
    int result = -EINVAL;
    if (!fstat(fd, &st) && S_ISREG(st.st_mode) && (uint64_t)st.st_size == size) {
        size_t done = 0;
        while (done < size) {
            ssize_t n = read(fd, (char *)data+done, size-done);
            if (n < 0 && errno == EINTR) continue;
            if (n <= 0) break;
            done += (size_t)n;
        }
        if (done == size) result = 0;
    }
    close(fd);
    return result;
}
int sspi_install_write(const char *directory, const char *name, const void *data, size_t size) {
    char path[128], temp[128];
    if (!sspi_install_directory(directory) || (strcmp(name,"request") && strcmp(name,"status"))) return -EINVAL;
    snprintf(path, sizeof(path), "%s/%s", directory, name);
    snprintf(temp, sizeof(temp), "%s/%s.tmp", directory, name);
    int fd = open(temp, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0600);
    if (fd < 0) return -errno;
    size_t done = 0;
    while (done < size) {
        ssize_t n = write(fd, (const char *)data+done, size-done);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        done += (size_t)n;
    }
    int result = done == size ? 0 : -EIO;
    if (!result && fsync(fd)) result = -errno;
    if (close(fd) && !result) result = -errno;
    if (!result && rename(temp, path)) result = -errno;
    if (result) unlink(temp);
    return result;
}
