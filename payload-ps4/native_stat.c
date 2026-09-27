#include "native_stat.h"
#include <string.h>
#include <sys/stat.h>

static void convert(struct stat *out, const NativeStat *in) {
    memset(out,0,sizeof(*out));
    out->st_dev=in->device; out->st_ino=in->inode; out->st_mode=in->mode; out->st_nlink=in->links;
    out->st_uid=in->uid; out->st_gid=in->gid; out->st_rdev=in->special_device;
    out->st_atim.tv_sec=in->accessed.seconds; out->st_atim.tv_nsec=in->accessed.nanoseconds;
    out->st_mtim.tv_sec=in->modified.seconds; out->st_mtim.tv_nsec=in->modified.nanoseconds;
    out->st_ctim.tv_sec=in->changed.seconds; out->st_ctim.tv_nsec=in->changed.nanoseconds;
    out->st_size=in->size; out->st_blocks=in->blocks; out->st_blksize=in->block_size;
    out->st_flags=in->flags; out->st_gen=in->generation;
    out->st_birthtim.tv_sec=in->created.seconds; out->st_birthtim.tv_nsec=in->created.nanoseconds;
}
int stat(const char *restrict path, struct stat *restrict info) {
    NativeStat native; int rc=rx_native_stat(path,&native); if (!rc) convert(info,&native); return rc;
}
int lstat(const char *restrict path, struct stat *restrict info) {
    NativeStat native; int rc=rx_native_lstat(path,&native); if (!rc) convert(info,&native); return rc;
}
int fstat(int fd, struct stat *info) {
    NativeStat native; int rc=rx_native_fstat(fd,&native); if (!rc) convert(info,&native); return rc;
}
