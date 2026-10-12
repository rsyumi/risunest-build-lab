#import <Foundation/Foundation.h>
#include <stdbool.h>
#include <string.h>

bool risunest_network_log_exclude_backup(const char *path) {
    @autoreleasepool {
        NSString *value = [[NSFileManager defaultManager] stringWithFileSystemRepresentation:path length:strlen(path)];
        if (value == nil) return false;
        NSURL *url = [NSURL fileURLWithPath:value isDirectory:YES];
        return [url setResourceValue:@YES forKey:NSURLIsExcludedFromBackupKey error:nil];
    }
}
