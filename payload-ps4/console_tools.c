#include "console_tools.h"
#include "installed_library.h"
#include "json.h"
#include "../payload/console_files.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/statfs.h>
#include <sched.h>
#include <orbis/libkernel.h>

static RxMutex icon_lock=RX_MUTEX_INIT;
static int system_info(int fd) {
    char out[4096]; Json j; json_init(&j,out,sizeof(out));
    OrbisKernelSwVersion version; memset(&version,0,sizeof(version)); version.Size=sizeof(version);
    json_add(&j,"{\"target\":\"ps4\",\"receiverVersion\":\"" VERSION "\",\"firmware\":");
    if (!sceKernelGetSystemSwVersion(&version) && version.Version) {
        char fw[32]; snprintf(fw,sizeof(fw),"%x.%02x",version.Version>>24,(version.Version>>16)&255); json_quote(&j,fw);
    } else json_add(&j,"null");
    json_add(&j,",\"sdkVersion\":null,\"model\":null,\"consoleName\":null,\"uptimeSeconds\":null,\"cpuTempC\":null,\"socTempC\":null,\"memory\":null,\"network\":null,\"runningTitleId\":null,\"storage\":[");
    const char *paths[]={"/user","/mnt/ext0"}; unsigned count=0;
    for (size_t i=0;i<2;i++) {
        struct statfs fs; if (ct_kind(paths[i])!=2 || statfs(paths[i],&fs) || !fs.f_bsize || !fs.f_blocks) continue;
        if ((uint64_t)fs.f_blocks>UINT64_MAX/(uint64_t)fs.f_bsize || (uint64_t)fs.f_bavail>(uint64_t)fs.f_blocks) continue;
        json_add(&j,"%s{\"label\":\"%s\",\"path\":\"%s\",\"totalBytes\":%llu,\"freeBytes\":%llu}",count++?",":"",i?"Extended":"Internal",paths[i],(unsigned long long)fs.f_blocks*fs.f_bsize,(unsigned long long)fs.f_bavail*fs.f_bsize);
    }
    json_add(&j,"],\"capabilities\":[\"pkg-preflight\",\"pkg-install\",\"url-install\",\"parallel-upload\",\"verify\",\"title-context\",\"progress-notifications\",\"install-control\",\"stop\",\"ps4\",\"installed-library-v1\",\"title-icons-v1\",\"system-info-v1\"],\"extras\":{}}");
    return j.failed ? text_reply(fd,RESP_ERROR,"System information exceeded its size limit.") : text_reply(fd,RESP_DATA,out);
}
int console_tools_request(int fd,uint8_t cmd,const uint8_t *b,size_t n) {
    if (cmd==CMD_SHELL_REFRESH || cmd==CMD_SYSTEM_INFO) {
        if (n) return text_reply(fd,RESP_ERROR,"This command requires an empty request.");
        return cmd==CMD_SHELL_REFRESH ? text_reply(fd,RESP_ERROR,"Restart your PS4 to see the new icons.") : system_info(fd);
    }
    if (!b || n<10 || b[9] || memchr(b,0,9) || !ct_valid_id((const char *)b)) return text_reply(fd,RESP_ERROR,"Invalid installed title ID.");
    if ((cmd==CMD_TITLE_ICON_GET && (n!=11 || b[10]>1)) || (cmd==CMD_TITLE_ICON_RESTORE && n!=10) ||
        (cmd==CMD_TITLE_ICON_SET && (n<=10 || n>CT_MAX_PNG+10))) return text_reply(fd,RESP_ERROR,"Invalid icon request length.");
    if (!installed_library_title_present((const char *)b)) return text_reply(fd,RESP_ERROR,"Refresh Library to confirm this title is installed.");
    rx_lock(&icon_lock); int result;
    if (cmd==CMD_TITLE_ICON_GET) {
        uint8_t *png=malloc(CT_MAX_PNG); size_t size=0;
        if (!png) result=text_reply(fd,RESP_ERROR,"Not enough memory to read the icon.");
        else { int rc=ct_icon_get(DATA_ROOT,(const char *)b,b[10]!=0,png,&size); result=rc ? text_reply(fd,RESP_ERROR,"The title icon is unavailable.") : reply(fd,RESP_DATA,png,(uint32_t)size); free(png); }
    } else {
        char text[512]; int rc=ct_icon_change(DATA_ROOT,"PS4",(const char *)b,b+10,n-10,cmd==CMD_TITLE_ICON_RESTORE,text,sizeof(text));
        result=text_reply(fd,rc?RESP_ERROR:RESP_DATA,text);
    }
    rx_unlock(&icon_lock); return result;
}
