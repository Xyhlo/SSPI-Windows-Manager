#ifndef SSPI_RUNTIME_H
#define SSPI_RUNTIME_H
/* The BinLoader invokes a function inside an existing process, not a new eboot. */
void payload_debug(const char *format, ...);
int payload_bootstrap(void);
#endif
