#ifndef SSPI_PLATFORM_H
#define SSPI_PLATFORM_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdatomic.h>

#define DATA_ROOT "/user/data/sspi-receiver"
#define UPLOAD_ROOT DATA_ROOT "/upload"
#define ART_ROOT DATA_ROOT "/artwork"
typedef struct { atomic_flag flag; } RxMutex;
#define RX_MUTEX_INIT { ATOMIC_FLAG_INIT }
void rx_sleep(unsigned milliseconds);
uint64_t rx_now(void);
uint64_t rx_wall_time(void);
static inline void rx_lock(RxMutex *m) { while (atomic_flag_test_and_set_explicit(&m->flag, memory_order_acquire)) rx_sleep(1); }
static inline void rx_unlock(RxMutex *m) { atomic_flag_clear_explicit(&m->flag, memory_order_release); }
enum { RX_READ, RX_CREATE, RX_APPEND };
int rx_open(const char *path, int mode);
int rx_close(int fd);
int rx_sync(int fd);
int rx_resize(int fd, uint64_t size);
int rx_size(int fd, uint64_t *size);
int rx_stat(const char *path, uint64_t *size, uint64_t *stamp);
int rx_mkdir(const char *path);
int rx_unlink(const char *path);
int rx_rename(const char *from, const char *to);
int64_t rx_pread(int fd, void *data, size_t size, uint64_t offset);
int64_t rx_pwrite(int fd, const void *data, size_t size, uint64_t offset);
int64_t rx_send(int fd, const void *data, size_t size);
int64_t rx_recv(int fd, void *data, size_t size, uint64_t deadline);
int rx_read_exact(int fd, void *data, size_t size, uint64_t offset);
int rx_write_exact(int fd, const void *data, size_t size, uint64_t offset);
int rx_atomic_file(const char *path, const void *data, size_t size);
#endif
