// LD_PRELOAD shim: raise a listen() backlog to $LISTEN_BACKLOG (default 4096).
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
int listen(int fd, int backlog) {
  static int (*real)(int, int);
  if (!real) real = (int (*)(int, int))dlsym(RTLD_NEXT, "listen");
  const char *e = getenv("LISTEN_BACKLOG");
  int b = e ? atoi(e) : 4096;
  return real(fd, backlog < b ? b : backlog);
}
