/*
 * WebKit Autoloader Installer - Main Entry Point
 *
 * This is a native PS5 ELF that starts a temporary HTTP server, opens the
 * browser to cache a page (or set of pages), installs the homescreen shortcut
 * once the cache is complete (via the /install route), then exits. On
 * subsequent launches from the homescreen, the cached content loads offline.
 *
 * This file handles: process init, signal setup, MHD lifecycle, shutdown.
 */

#include <microhttpd.h>
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/sysctl.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#include "wkali.h"
#include "http_server.h"
#include "ps5_launcher.h"
#include "file_registry.h"
#include "inflate.h"

/* SSPI handoff after successful installation; upstream installer remains GPL-3.0. */
static long long handoff_now_ms(void) {
    struct timeval now;
    if (gettimeofday(&now, NULL) != 0) return 0;
    return (long long)now.tv_sec * 1000 + now.tv_usec / 1000;
}

static int handoff_wait_writable(int socket, long long deadline) {
    for (;;) {
        long long left = deadline - handoff_now_ms();
        if (left <= 0 || left > 15000) return 0;
        struct timeval timeout = {left / 1000, (left % 1000) * 1000};
        fd_set sockets;
        FD_ZERO(&sockets);
        FD_SET(socket, &sockets);
        int result = select(socket + 1, NULL, &sockets, NULL, &timeout);
        if (result < 0 && errno == EINTR) continue;
        return result > 0;
    }
}

static int start_installed_runtime_once(void) {
    const FileEntry *entry = file_registry_find(
        "/app/" WKAL_FULL_VERSION "/payloads/payload.elf");
    if (!entry || entry->orig_size < 4 || entry->orig_size > 64 * 1024 * 1024)
        return -1;
    const unsigned char *payload = entry->data;
    size_t size = entry->size;
    unsigned char *inflated = NULL;
    if (entry->compressed) {
        inflated = malloc(entry->orig_size);
        if (!inflated) return -1;
        unsigned long destination = entry->orig_size, source = entry->size;
        if (puff(inflated, &destination, entry->data, &source) != 0 || destination != entry->orig_size) {
            free(inflated);
            return -1;
        }
        payload = inflated;
        size = destination;
    }
    if (size < 4 || memcmp(payload, "\177ELF", 4) != 0) {
        free(inflated);
        return -1;
    }

    int socket_fd = socket(AF_INET, SOCK_STREAM, 0);
    int result = -1;
    size_t sent = 0;
    if (socket_fd >= 0 && fcntl(socket_fd, F_SETFL, O_NONBLOCK) == 0) {
        struct sockaddr_in address;
        memset(&address, 0, sizeof(address));
        address.sin_family = AF_INET;
        address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        address.sin_port = htons(9021);
        const long long deadline = handoff_now_ms() + 15000;
        int connected = connect(socket_fd, (struct sockaddr *)&address, sizeof(address));
        if (connected == 0 || errno == EINPROGRESS) {
            int error = 0;
            socklen_t error_size = sizeof(error);
            if (handoff_wait_writable(socket_fd, deadline) &&
                getsockopt(socket_fd, SOL_SOCKET, SO_ERROR, &error, &error_size) == 0 && !error) {
                while (sent < size && handoff_wait_writable(socket_fd, deadline)) {
                    ssize_t amount = send(socket_fd, payload + sent, size - sent, 0);
                    if (amount < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)) continue;
                    if (amount <= 0) break;
                    sent += (size_t)amount;
                }
                result = sent == size ? 0 : -1;
            }
        }
    }
    if (socket_fd >= 0) close(socket_fd);
    free(inflated);
    wkali_log("[SSPI] Installed runtime handoff: %zu/%zu bytes sent; no automatic resend.\n", sent, size);
    return result;
}

static pid_t find_pid(const char *name) {
    int mib[4] = {1, 14, 8, 0};
    pid_t mypid = getpid();
    pid_t pid = -1;
    size_t buf_size;
    uint8_t *buf;

    if (sysctl(mib, 4, 0, &buf_size, 0, 0)) {
        wkali_log("[WKALI] sysctl failed\n");
        return -1;
    }

    if (!(buf = malloc(buf_size))) {
        wkali_log("[WKALI] malloc failed\n");
        return -1;
    }

    if (sysctl(mib, 4, buf, &buf_size, 0, 0)) {
        wkali_log("[WKALI] sysctl failed\n");
        free(buf);
        return -1;
    }

    /* KERN_PROC_ALL scan — raw offsets into FreeBSD 12's struct kinfo_proc
     * as exposed by the PS5 kernel: ki_pid at offset 72, ki_tdname at 447
     * (matches the layout used by the ps5-payload-dev SDK's klib). These
     * are ABI-specific; re-check if the kernel struct ever changes. */
    for (uint8_t *ptr = buf; ptr < (buf + buf_size);) {
        int ki_structsize = *(int *)ptr;
        pid_t ki_pid = *(pid_t *)&ptr[72];
        char *ki_tdname = (char *)&ptr[447];

        ptr += ki_structsize;
        if (!strcmp(name, ki_tdname) && ki_pid != mypid) {
            pid = ki_pid;
        }
    }

    free(buf);
    return pid;
}

/* PS5 System Calls (Internal) */
extern int sceNetCtlInit();
extern int sceUserServiceInitialize(void *);
extern int sceUserServiceGetForegroundUser(int *);

__attribute__((used)) volatile const char wkali_version_sig[] =
    "WKALI_VER:" WKAL_FULL_VERSION;

int main(void) {
    struct MHD_Daemon *daemon;
    pid_t pid;

    syscall(SYS_thr_set_name, -1, WKALI_THREAD_NAME);

    /* Kill previous installer instances */
    while ((pid = find_pid(WKALI_THREAD_NAME)) > 0) {
        if (kill(pid, SIGKILL)) {
            wkali_log("[WKALI] kill failed\n");
            return EXIT_FAILURE;
        }
        sleep(1);
    }

    wkali_log("[SSPI] Web launcher setup v%s (built %s) starting on port %d...\n",
                   WKAL_FULL_VERSION, WKAL_BUILD_TIME, WKALI_PORT);

    /* Initialize PS5 System Services */
    int err;
    if ((err = sceNetCtlInit()) == 0) {
        wkali_log("[WKALI] Network Controller initialized.\n");
    } else {
        wkali_log("[WKALI] sceNetCtlInit failed: 0x%08X\n", err);
    }

    int user_prio = 256;
    if ((err = sceUserServiceInitialize(&user_prio)) == 0) {
        wkali_log("[WKALI] User Service initialized.\n");
    } else {
        wkali_log("[WKALI] sceUserServiceInitialize failed: 0x%08X\n", err);
    }

    /* The homescreen app is installed/updated only AFTER the browser has
     * finished caching (via the /install route), so a shortcut is never
     * created for a partial cache. Nothing app-related happens at startup. */
    signal(SIGPIPE, SIG_IGN);
    signal(SIGHUP, SIG_IGN);
    signal(SIGTERM, SIG_IGN);

    /* Start the MHD daemon using a thread pool to handle concurrent AppCache requests. */
    daemon = MHD_start_daemon(MHD_USE_INTERNAL_POLLING_THREAD | MHD_USE_DEBUG,
                              WKALI_PORT, NULL, NULL, &http_on_request,
                              NULL, 
                              MHD_OPTION_THREAD_POOL_SIZE, (unsigned int)8,
                              MHD_OPTION_END);

    if (NULL == daemon) {
        wkali_log("[WKALI] Failed to start HTTP daemon!\n");
        wkali_notify("SSPI setup could not start\nHTTP server failed to start");
        return 1;
    }

    wkali_log("[WKALI] Server running. Waiting for the browser to cache content...\n");

    /* Query foreground user ID to pass to the frontend URL so the UI can
     * display the exact /user/home/<userid>/webkit/shell/ path in prompts. */
    int uid = -1;
    char uid_param[32] = "";
    if (sceUserServiceGetForegroundUser(&uid) == 0 && uid > 0) {
        snprintf(uid_param, sizeof(uid_param), "&uid=%08x", (unsigned int)uid);
    }

    /* Launch the browser at a versioned URL so the old AppCache master entry
     * for "/" is never served from the previous install. */
    char browser_url[256];
    snprintf(browser_url, sizeof(browser_url),
             "http://127.0.0.1:%d/?v=%s%s", WKALI_PORT, WKAL_FULL_VERSION, uid_param);
    ps5_launch_browser(browser_url);

    /* Main loop — runs until /install succeeds (which also installs the
     * homescreen app) and sets http_keep_running to 0 */
    int webkit_clear_attempts = 0;

    while (atomic_load(&http_keep_running)) {
        /* Check if the frontend requested a WebKit data clear */
        if (atomic_load(&webkit_data_cleared)) {
            atomic_store(&webkit_data_cleared, 0);
            webkit_clear_attempts++;

            if (webkit_clear_attempts <= 1) {
                /* Give the HTTP response time to flush before re-launching */
                usleep(500000);
                wkali_log("[WKALI] Re-launching browser after WebKit data clear (attempt %d)...\n",
                          webkit_clear_attempts);
                char retry_url[256];
                snprintf(retry_url, sizeof(retry_url),
                         "http://127.0.0.1:%d/?v=%s%s&retry=1",
                         WKALI_PORT, WKAL_FULL_VERSION, uid_param);
                ps5_launch_browser(retry_url);
            } else {
                wkali_log("[WKALI] WebKit clear already attempted %d time(s), not re-launching.\n",
                          webkit_clear_attempts);
            }
        }
        usleep(100000); /* 100ms sleep */
    }

    if (atomic_load(&install_completed)) {
        wkali_notify("SSPI setup saved. Starting payloads...");
    }
    wkali_log_wakeup();

    /* Give the /logs thread half a second to wake up and flush the final logs 
     * over the network before we aggressively kill the MHD daemon and all sockets. */
    usleep(500000); 

    if (daemon)
        MHD_stop_daemon(daemon);

    sleep(1);

    /* Failed/interrupted caching never reaches this handoff. No second exploit runs. */
    if (atomic_load(&install_completed) && start_installed_runtime_once() != 0) {
        wkali_notify("SSPI setup is saved. Payload startup did not complete; check the console before retrying.");
        return 1;
    }

    return 0;
}
