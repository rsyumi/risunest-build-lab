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
