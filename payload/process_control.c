/* Which processes the app may stop, shared by the PS5 and PS4 receivers.

   Measured on a PS5 at 11.20: payloads are children of elfldr.elf (sspi.elf, kstuff.elf,
   ftpsrv.elf, shadowmountplus.elf, payload.elf); a payload whose launcher exited is
   re-parented to PID 1 but keeps elfldr's auth ID (pldmgr.elf). System daemons descend
   from mini-syscore.elf / SceSysCore.elf, and the running app is eboot.bin under
   SceSysCore.elf. Anything not positively recognised is refused. */
#include "process_control.h"
#include <stdio.h>
#include <string.h>

static const PcProcess *find(const PcProcess *table, size_t count, int pid) {
    for (size_t i = 0; i < count; i++) if (table[i].pid == pid) return &table[i];
    return NULL;
}
static bool named(const PcProcess *p, const char *name) { return p && !strcmp(p->name, name); }
static bool payload_loader(const PcProcess *p) {
    return named(p,"elfldr.elf") || named(p,"elfldr") || named(p,"binloader.elf") || named(p,"binloader");
}
static bool standalone_service(const PcProcess *p) {
    if (!p || p->ppid!=1 || p->app_id || p->title[0]) return false;
    const char *names[]={"ftpsrv.elf","ftpsrv","klogsrv.elf","klogsrv","ps4debug.elf","ps4debug","ps4debug.bin"};
    for (size_t i=0;i<sizeof(names)/sizeof(*names);i++) if (named(p,names[i])) return true;
    return false;
}
static bool game_title(const char *t) {
    if ((strncmp(t, "CUSA", 4) && strncmp(t, "PPSA", 4)) || strlen(t) != 9) return false;
    for (int i = 4; i < 9; i++) if (t[i] < '0' || t[i] > '9') return false;
    return true;
}
static void say(char *reason, size_t cap, const char *text) { if (reason && cap) snprintf(reason, cap, "%s", text); }

PcKind pc_classify(const PcProcess *table, size_t count, int self, int pid, char *reason, size_t cap) {
    const PcProcess *p = find(table, count, pid);
    if (!p) { say(reason, cap, "That process is no longer running."); return PC_NONE; }
    if (pid <= 1) { say(reason, cap, "System processes can't be stopped from here."); return PC_NONE; }
    if (pid == self) { say(reason, cap, "This is the SSPI receiver. Use Stop in Tools > Payloads instead."); return PC_NONE; }
    if (payload_loader(p) || named(p,"GoldHEN") || named(p,"goldhen")) { say(reason, cap, "The ELF loader and GoldHEN are kept running so payloads can still be started."); return PC_NONE; }
    const PcProcess *parent = find(table, count, p->ppid);
    /* Apps and games: a CUSA/PPSA title, or eboot.bin started by the system's app launcher. */
    if (game_title(p->title) || (named(p, "eboot.bin") && (named(parent, "SceSysCore.elf") || named(parent, "SceSysCore")) && !(p->title[0] && !game_title(p->title)))) return PC_APP;
    /* Payloads: anything elfldr.elf started (directly or through another payload). */
    const PcProcess *at = parent;
    for (int hops = 0; at && hops < 32; hops++) {
        if (payload_loader(at)) return PC_PAYLOAD;
        if (at->pid <= 1 || at->ppid == at->pid) break;
        at = find(table, count, at->ppid);
    }
    // PS4 does not expose auth IDs. Recognize only known independent services;
    // never stop the process hosting a GoldHEN BIN thread.
    if (standalone_service(p)) return PC_PAYLOAD;
    /* A payload whose launcher exited lives on under PID 1 with elfldr's auth ID. */
    if (p->ppid == 1 && p->authid) {
        for (size_t i = 0; i < count; i++)
            if (named(&table[i], "elfldr.elf") && table[i].authid == p->authid) return PC_PAYLOAD;
    }
    say(reason, cap, "System processes can't be stopped from here. Only apps, games and identified separate payload processes can.");
    return PC_NONE;
}

const char *pc_kind_name(PcKind kind) { return kind == PC_APP ? "app" : kind == PC_PAYLOAD ? "payload" : NULL; }

int pc_parse(const uint8_t *body, size_t n, int *pid, char *action, char name[PC_NAME_MAX]) {
    if (!body || n < 7 || n > 5 + PC_NAME_MAX || body[n - 1]) return -1;
    uint32_t value = (uint32_t)body[0] | (uint32_t)body[1] << 8 | (uint32_t)body[2] << 16 | (uint32_t)body[3] << 24;
    if (value < 2 || value > 0x7fffffff || (body[4] != 's' && body[4] != 'e')) return -1;
    size_t length = n - 6;
    if (!length || memchr(body + 5, 0, length)) return -1;
    for (size_t i = 0; i < length; i++) if (body[5 + i] < 0x20 || body[5 + i] > 0x7e) return -1;
    *pid = (int)value; *action = (char)body[4];
    memcpy(name, body + 5, length); name[length] = 0;
    return 0;
}

PcMethod pc_method(PcKind kind, char action, uint32_t app_id) {
    if (action == 'e') return PC_SIGKILL;
    return kind == PC_APP && app_id ? PC_KILL_APP : PC_SIGTERM;
}
const char *pc_method_name(PcMethod method) { return method == PC_KILL_APP ? "close-app" : method == PC_SIGTERM ? "sigterm" : "sigkill"; }
