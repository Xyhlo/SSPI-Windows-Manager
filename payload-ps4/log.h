#ifndef SSPI_LOG_H
#define SSPI_LOG_H
#include "json.h"
void log_line(const char *format, ...);
void diagnostic(const char *format, ...);
void diagnostics_json(Json *j);
#endif
