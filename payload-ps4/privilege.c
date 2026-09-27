/* Isolate libjbc's freestanding typedefs and syscall names from the SDK headers. */
#include <jailbreak.h>
extern void diagnostic(const char *format, ...);
int privilege_apply(void) {
    struct jbc_cred cred;
    int rc=jbc_get_cred(&cred); diagnostic("jbc_get_cred rc=0x%08x",(unsigned)rc); if (rc) return rc;
    rc=jbc_jailbreak_cred(&cred); diagnostic("jbc_jailbreak_cred rc=0x%08x",(unsigned)rc); if (rc) return rc;
    rc=jbc_set_cred(&cred); diagnostic("jbc_set_cred rc=0x%08x",(unsigned)rc); return rc;
}
