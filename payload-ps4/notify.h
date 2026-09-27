#ifndef SSPI_NOTIFY_H
#define SSPI_NOTIFY_H
#include "platform.h"
typedef struct { char title_id[10], title[220], icon_url[1000]; } TitleContext;
typedef struct { char title_id[10]; const uint8_t *image; uint32_t image_size; } Artwork;
enum { NOTICE_RECEIVING=1, NOTICE_INSTALLING=2, NOTICE_INSTALLED=4, NOTICE_FAILED=8 };
int parse_artwork(const uint8_t *body, size_t size, Artwork *art);
int parse_context(const uint8_t *body, size_t size, TitleContext *context);
int notify_artwork(const uint8_t *body, size_t size);
int notify_context(const uint8_t *body, size_t size);
void notify_get_context(TitleContext *context);
void notify_receiving(const char *path);
void notify_event(const char *content_id, const char *title_id, const char *title, unsigned event, const char *error);
void notify_system(const char *text);
void notify_tick(void);
int platform_toast(const char *title_id, const char *message);
#endif
