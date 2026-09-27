#ifndef SSPI_PS4_CONSOLE_TOOLS_H
#define SSPI_PS4_CONSOLE_TOOLS_H
#include "proto.h"
int console_tools_request(int fd, uint8_t cmd, const uint8_t *body, size_t size);
#endif
