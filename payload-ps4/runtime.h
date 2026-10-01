#ifndef SSPI_RUNTIME_H
#define SSPI_RUNTIME_H
/* The BinLoader invokes a function inside an existing process, not a new eboot. */
void payload_debug(const char *format, ...);
int payload_bootstrap(void);
/* NULL when this console or process context does not export `name`. */
void *rx_optional_symbol(const char *name);
/* Diagnostics lock must be held. Calls one syscall with temporary libjbc creds,
   restoring even after a failed apply/call. Returns -1 on a credential failure;
   `result` still holds the syscall result (an opened fd must then be closed). */
int privilege_call(int (*call)(void *), void *context, int *result);
#endif
