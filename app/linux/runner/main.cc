#include <sys/resource.h>
#include <sys/stat.h>

#include "my_application.h"

int main(int argc, char** argv) {
  // A crash must not write this process's memory to disk: it holds the store key and decrypted
  // messages, and systemd-coredump would keep them in /var/lib/systemd/coredump for days, outside
  // the sealed store and the app lock (self-audit 2026-10-17, P7). A zero core limit makes
  // systemd-coredump store nothing (checked: "none" instead of the dump). Making the process
  // non-dumpable would also stop same-user ptrace, but xdg-desktop-portal then refuses every call
  // ("Unable to open /proc/PID/root"), and the file picker goes through it.
  struct rlimit no_core = {0, 0};
  setrlimit(RLIMIT_CORE, &no_core);
  // Owner-only files and folders: the data folder holds sealed state, but a world-readable one
  // showed other accounts its sizes, times and one file per contact (self-audit 2026-10-17,
  // round 10). The core also makes the folder itself 0700 when it opens it.
  umask(077);

  g_autoptr(MyApplication) app = my_application_new();
  return g_application_run(G_APPLICATION(app), argc, argv);
}
