#include <os/log.h>

void risunest_startup_log(const char *message) {
    os_log_error(OS_LOG_DEFAULT, "%{public}s", message);
}
