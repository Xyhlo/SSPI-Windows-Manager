#pragma once
#include <sys/types.h>
#include <time.h>
pid_t sspi_install_loader_waitpid(pid_t pid, int *status, int options);
const struct timespec *sspi_install_loader_timeout(void);
void sspi_install_loader_child(pid_t pid, void *stack);
void sspi_install_loader_release_stack(void);
void sspi_install_loader_abort(pid_t pid);
