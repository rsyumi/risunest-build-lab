#include <CoreFoundation/CoreFoundation.h>
#include <Security/Security.h>
#include <stdio.h>
#include <inttypes.h>
#include <sys/resource.h>
#include <unistd.h>

int main(void) {
    struct rlimit files;
    if (getrlimit(RLIMIT_NOFILE, &files) != 0) {
        perror("getrlimit");
        return 1;
    }
    printf("Simulator process limits: files-soft=%" PRIu64 " files-hard=%" PRIu64 " processors=%ld\n",
        (uint64_t)files.rlim_cur, (uint64_t)files.rlim_max, sysconf(_SC_NPROCESSORS_ONLN));
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
