/* SSPI integration for itsPLK's GPL-3.0 Payload Manager and unified autoloader. */
#ifndef SSPI_MANAGER_RUNTIME_H
#define SSPI_MANAGER_RUNTIME_H

#include "sspi_contract.h"
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

static inline long long sspi_now_ms(void) {
    struct timeval value;
    if (gettimeofday(&value, NULL) != 0) return 0;
    return (long long)value.tv_sec * 1000 + value.tv_usec / 1000;
}

static inline int sspi_socket_wait(int sock, int writing, long long deadline) {
    for (;;) {
        long long left = deadline - sspi_now_ms();
        if (left <= 0 || left > 400) return 0;
        struct timeval timeout = {left / 1000, (left % 1000) * 1000};
        fd_set descriptors;
        FD_ZERO(&descriptors);
        FD_SET(sock, &descriptors);
        int result = select(sock + 1, writing ? NULL : &descriptors,
                            writing ? &descriptors : NULL, NULL, &timeout);
        if (result < 0 && errno == EINTR) continue;
        return result > 0;
    }
}

/* Only talks to the console's loopback manager HTTP port, never a loader port. */
static inline int sspi_manager_request(int activate) {
    static const char identity_request[] =
        "GET /sspi/identity HTTP/1.1\r\nHost: 127.0.0.1:8084\r\nConnection: close\r\n\r\n";
    static const char activate_request[] =
        "POST /sspi/activate HTTP/1.1\r\nHost: 127.0.0.1:8084\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const char *request = activate ? activate_request : identity_request;
    const size_t request_size = strlen(request);
    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if (sock < 0) return 0;
#ifdef SO_NOSIGPIPE
    int no_sigpipe = 1;
    setsockopt(sock, SOL_SOCKET, SO_NOSIGPIPE, &no_sigpipe, sizeof(no_sigpipe));
#endif
    if (fcntl(sock, F_SETFL, O_NONBLOCK) < 0) {
        close(sock);
        return 0;
    }
    const long long deadline = sspi_now_ms() + 400;
    struct sockaddr_in address;
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_port = htons(SSPI_MANAGER_PORT);
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    int result = connect(sock, (struct sockaddr *)&address, sizeof(address));
    if (result < 0 && errno != EINPROGRESS) {
        close(sock);
        return 0;
    }
    int error = 0;
    socklen_t error_size = sizeof(error);
    if (!sspi_socket_wait(sock, 1, deadline) ||
        getsockopt(sock, SOL_SOCKET, SO_ERROR, &error, &error_size) < 0 || error) {
        close(sock);
        return 0;
    }
    size_t sent = 0;
    while (sent < request_size) {
        if (!sspi_socket_wait(sock, 1, deadline)) break;
        ssize_t amount = send(sock, request + sent, request_size - sent, 0);
        if (amount < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)) continue;
        if (amount <= 0) break;
        sent += (size_t)amount;
    }
    char response[2048];
    size_t received = 0;
    int ready = 0;
    while (sent == request_size && received < sizeof(response)) {
        if (!sspi_socket_wait(sock, 0, deadline)) break;
        ssize_t amount = recv(sock, response + received, sizeof(response) - received, 0);
        if (amount < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)) continue;
        if (amount <= 0) break;
        received += (size_t)amount;
        if (sspi_http_identity_ready(response, received)) {
            ready = 1;
            break;
        }
    }
    close(sock);
    return ready;
}

static inline int sspi_probe_manager(void) { return sspi_manager_request(0); }
static inline int sspi_activate_manager(void) { return sspi_manager_request(1); }

#endif
