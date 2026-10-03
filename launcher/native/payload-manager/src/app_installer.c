/*
 * SSPI's single home-screen entry, based on Payload Manager's GPL-3.0
 * app installer by itsPLK and the original ftpsrv work by John Tornblom.
 * WKAL00001 is installed only by the cache-complete WebKit installer.
 */
#include <stdio.h>
#include <sys/stat.h>
#include "app_installer.h"
#include "pldmgr.h"
#include "sspi_shortcut_contract.h"

int sceAppInstUtilInitialize(void);
int sceAppInstUtilTerminate(void);
int sceAppInstUtilAppUnInstall(const char *);

static enum sspi_shortcut_kind read_shortcut(const char *title_id) {
    char path[128], text[4096];
    snprintf(path, sizeof(path), "/user/app/%s/sce_sys/param.json", title_id);
    struct stat st;
    if (lstat(path, &st) != 0 || !S_ISREG(st.st_mode)) return SSPI_SHORTCUT_UNKNOWN;
    FILE *file = fopen(path, "rb");
    if (!file) return SSPI_SHORTCUT_UNKNOWN;
    size_t size = fread(text, 1, sizeof(text), file);
    int failed = ferror(file);
    int extra = fgetc(file);
    fclose(file);
    if (failed || extra != EOF || size == sizeof(text)) return SSPI_SHORTCUT_UNKNOWN;
    return sspi_classify_shortcut(text, size);
}

int pldmgr_install_app_if_needed(void) {
    /* Never create PLDM00001 or rewrite AUTO_INSTALL_APP. A standalone Manager
     * without the cached SSPI launcher keeps working through its browser URL. */
    if (!sspi_should_retire_manager_shortcut(read_shortcut("WKAL00001"),
                                            read_shortcut("PLDM00001"))) return 0;
    int result = sceAppInstUtilInitialize();
    if (result != 0) {
        pldmgr_log("[SSPI] Could not consolidate home-screen shortcuts: 0x%08X\n", result);
        return -1;
    }
    /* Remove only the verified browser shortcut. /data/pldmgr is never touched. */
    result = sceAppInstUtilAppUnInstall("PLDM00001");
    sceAppInstUtilTerminate();
    if (result != 0) {
        pldmgr_log("[SSPI] Could not remove the duplicate Manager shortcut: 0x%08X\n", result);
        return -1;
    }
    pldmgr_log("[SSPI] Home-screen entry consolidated as WKAL00001; Manager data retained.\n");
    return 0;
}
