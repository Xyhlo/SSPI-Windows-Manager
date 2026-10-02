/* SSPI listener status; GPL-3.0 integration with John Tornblom's elfldr. */
#ifndef SSPI_LOADER_CONTRACT_H
#define SSPI_LOADER_CONTRACT_H

#include <stddef.h>
#include <stdio.h>

#define SSPI_LOADER_STATUS_PATH "/data/pldmgr/sspi-elfldr.status"

struct sspi_loader_record {
    long pid;
    long long start_seconds;
    long start_microseconds;
    int port;
    int ready;
};

static inline int sspi_parse_loader_record(const char *text, struct sspi_loader_record *record) {
    int consumed = 0;
    if (sscanf(text, "SSPI_ELFLDR/1 %ld %lld %ld %d %d%n", &record->pid,
               &record->start_seconds, &record->start_microseconds,
               &record->port, &record->ready, &consumed) != 5 || consumed == 0 ||
        record->pid <= 0 || record->start_seconds <= 0 ||
        record->start_microseconds < 0 || record->start_microseconds >= 1000000 ||
        record->port != 9021 || (record->ready != 0 && record->ready != 1))
        return 0;
    for (const char *tail = text + consumed; *tail; tail++)
        if (*tail != '\r' && *tail != '\n') return 0;
    return 1;
}

static inline int sspi_loader_record_matches(const struct sspi_loader_record *record,
                                              long pid, long long seconds, long microseconds) {
    return record->pid == pid && record->start_seconds == seconds &&
           record->start_microseconds == microseconds;
}

#endif
