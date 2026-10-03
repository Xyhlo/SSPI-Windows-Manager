/* SSPI integration with GPL-3.0 Payload Manager. */
#ifndef SSPI_SESSION_CONTRACT_H
#define SSPI_SESSION_CONTRACT_H

#include <stddef.h>
#include <stdio.h>

/* A saved flag cannot establish readiness after a reboot or process restart. */
static inline int sspi_session_ready(int serving, unsigned long uid,
                                     unsigned long euid, long pid,
                                     long long seconds, long microseconds) {
    return serving && uid == 0 && euid == 0 && pid > 0 && seconds > 0 &&
           microseconds >= 0 && microseconds < 1000000;
}

static inline int sspi_session_json(char *output, size_t size, int serving,
                                    unsigned long uid, unsigned long euid,
                                    long pid, long long seconds, long microseconds) {
    int ready = sspi_session_ready(serving, uid, euid, pid, seconds, microseconds);
    char session[96] = "";
    if (ready) snprintf(session, sizeof(session), "%ld-%lld-%ld", pid, seconds, microseconds);
    int written = snprintf(output, size,
        "{\"edition\":\"sspi-payload-manager\",\"protocol\":1,\"ready\":%s,"
        "\"sessionId\":\"%s\",\"jailbreak\":\"%s\",\"evidence\":\"%s\","
        "\"launcherTitleId\":\"WKAL00001\"}",
        ready ? "true" : "false", session, ready ? "active" : "unknown",
        ready ? "privileged-manager-process" : "none");
    return written >= 0 && (size_t)written < size ? written : -1;
}

#endif
