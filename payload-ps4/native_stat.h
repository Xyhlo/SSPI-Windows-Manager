#ifndef SSPI_NATIVE_STAT_H
#define SSPI_NATIVE_STAT_H
#include <stddef.h>
#include <stdint.h>

/* PS4 libkernel uses the FreeBSD 9 stat ABI. The local OpenOrbis headers
   select a 32-bit mode_t, so their struct stat cannot cross this boundary. */
typedef struct { int64_t seconds, nanoseconds; } NativeStatTime;
typedef struct {
    uint32_t device, inode;
    uint16_t mode, links;
    uint32_t uid, gid, special_device;
    NativeStatTime accessed, modified, changed;
    int64_t size, blocks;
    uint32_t block_size, flags, generation, spare;
    NativeStatTime created;
} NativeStat;
_Static_assert(sizeof(NativeStat)==120,"PS4 native stat size");
_Static_assert(offsetof(NativeStat,mode)==8,"PS4 native mode offset");
_Static_assert(offsetof(NativeStat,modified)==40,"PS4 native modification time offset");
_Static_assert(offsetof(NativeStat,size)==72,"PS4 native file size offset");
_Static_assert(offsetof(NativeStat,blocks)==80,"PS4 native block count offset");

int rx_native_stat(const char *path, NativeStat *info);
int rx_native_lstat(const char *path, NativeStat *info);
int rx_native_fstat(int fd, NativeStat *info);
#endif
