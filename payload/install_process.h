#pragma once
#include "install_protocol.h"

/* The receiver serializes these calls with its installation mutex. */
int sspi_install_start(const char *path, const char *title, const char *icon, int verified_fd);
void sspi_install_poll(void);
bool sspi_install_busy(void);
const SspiInstallSnapshot *sspi_install_snapshot(void);
int sspi_install_pid(void);
bool sspi_install_running(void);
bool sspi_install_loading(void);
const char *sspi_install_path(void);
int sspi_install_confirm(const char *content_id,const char *path,const char *attempt);
