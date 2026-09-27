#ifndef SSPI_TASKS_H
#define SSPI_TASKS_H
#include "json.h"
#define TASK_ROOT DATA_ROOT "/tasks"
#define TASK_CONFLICT "Another download for this content is already in the PS4 download queue. Remove it there, then retry."
typedef struct {
    int task_id; char content_id[37]; enum PkgKind kind;
    uint64_t size, declared_size, created; bool has_declared_size;
    char header_sha256[65], digest[65];
} TaskRecord;
int task_record_path(const char *cid, enum PkgKind kind, char *path, size_t cap);
int task_record_format(const TaskRecord *record, char *text, size_t cap);
int task_record_parse(const char *text, size_t size, TaskRecord *record);
int task_record_save(const TaskRecord *record);
int task_record_load(const char *cid, enum PkgKind kind, TaskRecord *record);
bool task_record_matches(const TaskRecord *record, int task, const UrlRequest *request);
int task_record_remove(const char *cid, enum PkgKind kind, int task);
#endif
