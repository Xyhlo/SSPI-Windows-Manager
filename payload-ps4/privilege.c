/* Isolate libjbc's freestanding typedefs and syscall names from the SDK headers. */
#include <jailbreak.h>
#include <errno.h>
#include "runtime.h"
extern void diagnostic(const char *format, ...);
int privilege_apply(void) {
    struct jbc_cred cred;
    int rc=jbc_get_cred(&cred); diagnostic("jbc_get_cred rc=0x%08x",(unsigned)rc); if (rc) return rc;
    rc=jbc_jailbreak_cred(&cred); diagnostic("jbc_jailbreak_cred rc=0x%08x",(unsigned)rc); if (rc) return rc;
    rc=jbc_set_cred(&cred); diagnostic("jbc_set_cred rc=0x%08x",(unsigned)rc); return rc;
}

/* Set once a credential change fails: the process may be left half-changed, so
   no later request elevates again until the receiver is reloaded. */
static int privilege_broken;

int privilege_call(int (*call)(void *), void *context, int *result) {
    struct jbc_cred saved, elevated;
    *result=-1;
    if (privilege_broken) { errno=EIO; return -1; }
    if (jbc_get_cred(&saved)) { errno=EIO; return -1; }
    elevated=saved;
    if (jbc_jailbreak_cred(&elevated)) { errno=EIO; return -1; }
    int applied=jbc_set_cred(&elevated), error=EIO;
    if (!applied) { errno=0; *result=call(context); error=errno; }
    /* libjbc can fail after a partial write. Restore after every set attempt,
       before allocation, logging, reading an fd, or another diagnostic call. */
    int restored=jbc_set_cred(&saved);
    if (restored) restored=jbc_set_cred(&saved);
    if (restored || applied) { privilege_broken=1; errno=EIO; return -1; }
    errno=error; return 0;
}
