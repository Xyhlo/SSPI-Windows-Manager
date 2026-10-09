#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define SSPI_INSTALL_MAGIC UINT32_C(0x53495032)
#define SSPI_INSTALL_ROOT "/data/SSPI/install-"
#define SSPI_INSTALL_AUTHID UINT64_C(0x4801000000000013)
#define SSPI_INSTALL_PATH 2049

/* Private, versioned IPC between two ELFs built together, never network input. */
typedef struct {
    uint32_t magic, size;
    char attempt[32];
    int32_t parent;
    uint32_t reserved;
    uint64_t file_size, file_inode;
    char path[SSPI_INSTALL_PATH], title[256], icon[1024], content_id[48];
} SspiInstallRequest;

enum { SSPI_INSTALL_RUNNING, SSPI_INSTALL_REJECTED, SSPI_INSTALL_COMPLETE,
       SSPI_INSTALL_FAILED, SSPI_INSTALL_UNCONFIRMED };
enum { SSPI_SUBMIT_NONE, SSPI_SUBMIT_CALLING, SSPI_SUBMIT_ACCEPTED, SSPI_SUBMIT_REJECTED };
typedef struct {
    uint32_t magic, size;
    char attempt[32];
    uint64_t sequence;
    uint32_t outcome, submission;
    int32_t api_code, install_code, status_code, error_code, terminate_code, auth_restore_code;
    uint32_t progress;
    int32_t helper_pid;
    uint32_t helper_running, ownership_released;
    uint64_t downloaded, total;
    char phase[32], content_id[48], status[16], error[512], path[SSPI_INSTALL_PATH];
} SspiInstallSnapshot;

uint64_t sspi_install_millis(void);
bool sspi_install_directory(const char *directory);
bool sspi_install_package_path(const char *path);
bool sspi_install_content_id(const char *id);
bool sspi_install_package(int fd, uint64_t size, char content_id[48]);
int sspi_install_read(const char *directory, const char *name, void *data, size_t size);
int sspi_install_write(const char *directory, const char *name, const void *data, size_t size);
