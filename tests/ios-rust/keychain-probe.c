#include <CoreFoundation/CoreFoundation.h>
#include <Security/Security.h>
#include <stdio.h>
#include <stdlib.h>
#include <inttypes.h>
#include <sys/resource.h>
#include <unistd.h>

int main(void) {
    const char *threads = getenv("RUST_TEST_THREADS");
    char *end = NULL;
    long thread_count = threads ? strtol(threads, &end, 10) : 0;
    if (thread_count < 1 || !end || *end != '\0') {
        fprintf(stderr, "RUST_TEST_THREADS was not passed to the simulator process\n");
        return 1;
    }
    struct rlimit files;
    if (getrlimit(RLIMIT_NOFILE, &files) != 0) {
        perror("getrlimit");
        return 1;
    }
    printf("Simulator process limits: files-soft=%" PRIu64 " files-hard=%" PRIu64 " processors=%ld rust-test-threads=%ld\n",
        (uint64_t)files.rlim_cur, (uint64_t)files.rlim_max, sysconf(_SC_NPROCESSORS_ONLN), thread_count);
    CFUUIDRef uuid = CFUUIDCreate(NULL);
    CFStringRef account = CFUUIDCreateString(NULL, uuid);
    const UInt8 bytes[] = "synthetic-keychain-probe";
    CFDataRef data = CFDataCreate(NULL, bytes, sizeof(bytes) - 1);
    const void *keys[] = { kSecClass, kSecAttrService, kSecAttrAccount, kSecValueData };
    const void *values[] = {
        kSecClassGenericPassword, CFSTR("io.github.rsyumi.risunest.rust-tests.probe"), account, data
    };
    CFDictionaryRef query = CFDictionaryCreate(NULL, keys, values, 4,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    OSStatus added = SecItemAdd(query, NULL);
    CFDictionaryRef identity = CFDictionaryCreate(NULL, keys, values, 3,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    OSStatus found = SecItemCopyMatching(identity, NULL);
    OSStatus removed = SecItemDelete(identity);
    printf("Synthetic keychain preflight: add=%d find=%d remove=%d\n",
        (int)added, (int)found, (int)removed);
    CFRelease(identity);
    CFRelease(query);
    CFRelease(data);
    CFRelease(account);
    CFRelease(uuid);
    return added == errSecSuccess && found == errSecSuccess && removed == errSecSuccess ? 0 : 1;
}
