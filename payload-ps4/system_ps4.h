#ifndef SSPI_PS4_SYSTEM_H
#define SSPI_PS4_SYSTEM_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
/* -1 unavailable, -2 the kernel's process record layout is not the one this receiver reads. */
int ps4_processes_json(char *out, size_t cap);
/* Stops an app (process-control-v1); -1 with a sentence in `error` when refused. */
int ps4_process_control(const uint8_t *body, size_t n, char *out, size_t cap, char *error, size_t error_cap);
void ps4_process_summary(unsigned *count, char running[10]);
bool ps4_temperatures(int *cpu, int *soc);
long ps4_cpu_mhz(void);
bool ps4_memory(uint64_t *total, uint64_t *free_bytes);
bool ps4_uptime(uint64_t *seconds);
bool ps4_host_name(char *out, size_t cap);
void ps4_network(char ip[20], char mac[20], char interface_name[20]);
int ps4_mounts_json(char *out, size_t cap, size_t *at);
#endif
