#import <WebKit/WebKit.h>
#include <stdbool.h>

void risunest_clear_webview(void *view, void *context, void (*completed)(void *, bool)) {
    WKWebView *webview = (__bridge WKWebView *)view;
    WKWebsiteDataStore *store = webview.configuration.websiteDataStore;
    if (!store) {
        completed(context, false);
        return;
    }
    [store removeDataOfTypes:[WKWebsiteDataStore allWebsiteDataTypes]
              modifiedSince:[NSDate distantPast]
          completionHandler:^{ completed(context, true); }];
}

#if TARGET_OS_OSX
#include <CoreFoundation/CoreFoundation.h>

// Removes a named profile's store, which no WebView may be using. A store that
// was never created needs no removal.
void risunest_remove_webview_store(const unsigned char *identifier, void *context, void (*completed)(void *, bool)) {
    if (@available(macOS 14.0, *)) {
        NSUUID *target = [[NSUUID alloc] initWithUUIDBytes:identifier];
        [WKWebsiteDataStore fetchAllDataStoreIdentifiers:^(NSArray<NSUUID *> *identifiers) {
            if (![identifiers containsObject:target]) {
                completed(context, true);
                return;
            }
            [WKWebsiteDataStore removeDataStoreForIdentifier:target completionHandler:^(NSError *error) {
                completed(context, error == nil);
            }];
        }];
    } else {
        completed(context, true);
    }
}

void risunest_clear_default_webview_store(void *context, void (*completed)(void *, bool)) {
    [[WKWebsiteDataStore defaultDataStore] removeDataOfTypes:[WKWebsiteDataStore allWebsiteDataTypes]
                                               modifiedSince:[NSDate distantPast]
                                           completionHandler:^{ completed(context, true); }];
}

// Completion handlers arrive on the main queue, which a process without an
// application event loop drains only while its main run loop runs.
void risunest_run_loop_step(double seconds) {
    CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, true);
}
#endif
