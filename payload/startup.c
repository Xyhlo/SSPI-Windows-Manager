/* Copyright (C) 2024 John Törnblom

This program is free software; you can redistribute it and/or modify it
under the terms of the GNU General Public License as published by the
Free Software Foundation; either version 3, or (at your option) any
later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program; see the file COPYING. If not, see
<http://www.gnu.org/licenses/>.  */

#include "kernel.h"
#include "klog.h"
#include "patch.h"
#include "payload.h"
#include "rtld.h"
#include "rtld_dlfcn.h"
#include "rtld_payload.h"
#include "syscall.h"

/* Based on ps5-payload-dev/sdk f7fd02e6e195902a449b5e664917be8c01888b34.
 * Keep diagnostics independent of libc/rtld: stdout is the loader socket.
 * FreeBSD sendto(133), MSG_DONTWAIT | MSG_NOSIGNAL: a detached or slow loader
 * must never block startup or terminate the receiver with SIGPIPE. */
void
sspi_startup_note(const char *stage, int result) {
  char line[160];
  const char prefix[] = "[SSPI startup] ";
  const char hex[] = "0123456789abcdef";
  unsigned int code = (unsigned int)result;
  unsigned int n = 0;
  for(unsigned int i=0; i<sizeof(prefix)-1; i++) line[n++] = prefix[i];
  for(unsigned int i=0; stage[i] && n<130; i++) line[n++] = stage[i];
  line[n++] = ' '; line[n++] = '0'; line[n++] = 'x';
  for(int shift=28; shift>=0; shift-=4) line[n++] = hex[(code >> shift) & 15];
  line[n++] = '\n';
  (void)__crt_syscall(133, 1, line, (unsigned long)n, 0x20080, 0, 0);
}


/**
 * Dependencies provided by the ELF linker.
 **/
extern unsigned char __bss_start[] __attribute__((weak));
extern unsigned char __bss_end[] __attribute__((weak));


/**
 * Entry point to the main program.
 **/
extern int main(int argc, char* argv[], char *envp[]);


/**
 * Remember the args passed to _start and the cpu state.
 **/
static payload_args_t* payload_args = 0;
static void* jmpbuf[32];


/**
 * Initialize payload runtime.
 **/
static int
payload_init(payload_args_t *args) {
  int *__isthreaded = 0;
  int error = 0;

  if((error=__crt_syscall_init(args))) {
    return error;
  }
  sspi_startup_note("kernel init begin", 0);
  if((error=__kernel_init(args))) {
    sspi_startup_note("kernel init failed", error);
    return error;
  }
  sspi_startup_note("kernel init complete", 0);
  if((error=__klog_init())) {
    sspi_startup_note("kernel log init failed", error);
    return error;
  }

  if(!KERNEL_DLSYM(0x2, __isthreaded)) {
    sspi_startup_note("libc thread symbol missing", -1);
    klog_puts("Unable to resolve the symbol '__isthreaded'");
    return -1;
  }
  *__isthreaded = 1;

  if((error=__patch_init())) {
    sspi_startup_note("runtime patch init failed", error);
    klog_puts("Unable to initialize patches");
    return error;
  }
  if((error=__rtld_init())) {
    sspi_startup_note("runtime linker init failed", error);
    klog_puts("Unable to initialize rtld");
    return error;
  }

  sspi_startup_note("runtime init complete", 0);
  return 0;
}


/**
 * Run the payload.
 **/
static int
payload_run(void) {
  const char* __progname = 0;
  char** (*getargv)(void) = 0;
  int (*getargc)(void) = 0;
  rtld_lib_t* lib = 0;
  char** environ = 0;
  char** argv = 0;
  int argc = 0;
  int err = 0;

  if((KERNEL_DLSYM(0x1, getargc) || KERNEL_DLSYM(0x2001, getargc)) &&
     (KERNEL_DLSYM(0x1, getargv) || KERNEL_DLSYM(0x2001, getargv))) {
    argc = getargc();
    argv = getargv();
  }

  if(!(KERNEL_DLSYM(0x1, environ))) {
    if(!(KERNEL_DLSYM(0x2001, environ))) {
      environ = 0;
    }
  }

  if(!(KERNEL_DLSYM(0x1, __progname))) {
    if(!(KERNEL_DLSYM(0x2001, __progname))) {
      __progname = "";
    }
  }

  if(!(lib=__rtld_payload_new(__progname))) {
    sspi_startup_note("payload library allocation failed", -1);
    return -1;
  }

  __rtld_dlfcn_setroot(lib);
  sspi_startup_note("load libraries begin", 0);
  if((err=__rtld_lib_open(lib))) {
    sspi_startup_note("load libraries failed", err);
    __rtld_lib_destroy(lib);
    return err;
  }

  // run .init constructors
  if((err=__rtld_lib_init(lib, argc, argv, environ))) {
    sspi_startup_note("library constructors failed", err);
    __rtld_lib_close(lib);
    __rtld_lib_destroy(lib);
    return err;
  }

  // run the actual payload
  sspi_startup_note("enter main", 0);
  err = main(argc, argv, environ);
  sspi_startup_note("main returned", err);
  if(payload_args->payloadout) {
    *payload_args->payloadout = err;
  }

  // run .fini destructors
  if((err=__rtld_lib_fini(lib))) {
    __rtld_lib_close(lib);
    __rtld_lib_destroy(lib);
    return err;
  }

  err = __rtld_lib_close(lib);
  __rtld_lib_destroy(lib);

  return err;
}


/**
 * Terminate the payload.
 **/
static int
payload_terminate(void) {
  void (*exit)(int) = 0;
  int exit_code = 0;

  // we are running inside a hijacked process, just return
  if(kernel_dynlib_dlsym(-1, 0x2001, "sceKernelDlsym")) {
    return exit_code;
  }

  if(payload_args->payloadout) {
    exit_code = *payload_args->payloadout;
  }

  // resolve and run exit
  if(KERNEL_DLSYM(0x2, exit)) {
    exit(exit_code);
  }

  // should not happend
  __builtin_trap();

  return -1;
}


/**
 * Exit the payload by transfering the flow of control back to _start().
 **/
void
payload_exit(int exit_code) {
  if(payload_args->payloadout) {
    *payload_args->payloadout = exit_code;
  }
  __builtin_longjmp(jmpbuf, 1);
}


/**
 * Provide a convenience function for accessing the payload args.
 **/
payload_args_t*
payload_get_args(void) {
  return payload_args;
}


/**
 * Entry-point invoked by the ELF loader.
 **/
int
__crt_start(payload_args_t *args) {
  int err;
  if(!args || !args->sys_dynlib_dlsym) {
    return -1;
  }

  // clear .bss section
  for(unsigned char* bss=__bss_start; bss<__bss_end; bss++) {
    *bss = 0;
  }

  payload_args = args;

  // init payload runtime
  if((err=payload_init(args))) {
    if(args->payloadout) {
      *args->payloadout = err;
    }
    /* Kernel/library state may be incomplete. Return to the loader without
       calling a termination routine that resolves symbols through that state. */
    return err;
  }

  // run payload
  if(!__builtin_setjmp(jmpbuf)) {
    if((err=payload_run())) {
      if(args->payloadout) {
	*args->payloadout = err;
      }
    }
  }

  // terminate payload
  return payload_terminate();
}
