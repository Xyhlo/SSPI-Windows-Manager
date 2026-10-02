/* SSPI listener status; GPL-3.0 integration with John Tornblom's elfldr. */
#ifndef SSPI_LOADER_STATUS_H
#define SSPI_LOADER_STATUS_H

#include "sspi_loader_contract.h"
#include <errno.h>
#include <stddef.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/user.h>
#include <unistd.h>

static inline int sspi_loader_process(long pid, struct kinfo_proc *info) {
    if (pid <= 0 || (long)(pid_t)pid != pid) return 0;
    int mib[] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, (int)pid};
    size_t size = sizeof(*info);
    memset(info, 0, size);
    if (sysctl(mib, 4, info, &size, NULL, 0) != 0 ||
        size < offsetof(struct kinfo_proc, ki_start) + sizeof(info->ki_start))
        return 0;
    return info->ki_pid == (pid_t)pid && info->ki_start.tv_sec > 0;
}

static inline void sspi_write_loader_status(int port, int ready) {
    struct kinfo_proc info;
    if (!sspi_loader_process(getpid(), &info)) return;
    if (mkdir("/data/pldmgr", 0777) != 0 && errno != EEXIST) return;
    char temporary[160];
    snprintf(temporary, sizeof(temporary), SSPI_LOADER_STATUS_PATH ".%ld.tmp", (long)getpid());
    FILE *file = fopen(temporary, "wb");
    if (!file) return;
    int result = fprintf(file, "SSPI_ELFLDR/1 %ld %lld %ld %d %d\n", (long)getpid(),
                         (long long)info.ki_start.tv_sec, (long)info.ki_start.tv_usec,
                         port, ready ? 1 : 0);
    if (fclose(file) != 0 || result < 0 || rename(temporary, SSPI_LOADER_STATUS_PATH) != 0)
        unlink(temporary);
}

static inline int sspi_read_loader_status(void) {
    char text[192];
    FILE *file = fopen(SSPI_LOADER_STATUS_PATH, "rb");
    if (!file) return -1;
    const size_t size = fread(text, 1, sizeof(text) - 1, file);
    const int extra = fgetc(file);
    const int failed = ferror(file);
    fclose(file);
    if (failed || extra != EOF || !size) return -1;
    text[size] = '\0';
    if (strlen(text) != size) return -1;
    struct sspi_loader_record record;
    struct kinfo_proc info;
    if (!sspi_parse_loader_record(text, &record) || !sspi_loader_process(record.pid, &info) ||
        !sspi_loader_record_matches(&record, (long)info.ki_pid,
                                    (long long)info.ki_start.tv_sec, (long)info.ki_start.tv_usec))
        return -1;
    return record.ready;
}

#endif
