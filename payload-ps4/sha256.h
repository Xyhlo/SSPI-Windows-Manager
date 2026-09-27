#ifndef SSPI_SHA256_H
#define SSPI_SHA256_H
#include <stddef.h>
#include <stdint.h>
void sha256_hex(const void *data, size_t size, char hex[65]);
void digest_hex(const uint8_t digest[32], char hex[65]);
int hash_normalize(char *hex);
#endif
