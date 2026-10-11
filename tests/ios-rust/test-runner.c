#include <stdio.h>
#include <sys/resource.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "Usage: test-runner EXECUTABLE [ARGUMENTS...]\n");
        return 1;
    }
    struct rlimit files;
    if (getrlimit(RLIMIT_NOFILE, &files) != 0) {
        perror("getrlimit");
        return 1;
    }
    if (files.rlim_cur < 1024) {
        files.rlim_cur = 1024;
        if (setrlimit(RLIMIT_NOFILE, &files) != 0) {
            perror("setrlimit");
            return 1;
        }
    }
    // exec preserves this process's limits and the test executable's exit status.
    execv(argv[1], &argv[1]);
    perror("execv");
    return 1;
}
