#import <AppKit/AppKit.h>
#import <objc/runtime.h>

static void (*requestProbe)(void);
static BOOL pending;
static unsigned replies;

static NSApplicationTerminateReply probeShouldTerminate(id delegate, SEL selector, NSApplication *sender) {
    if (!pending) {
        pending = YES;
        requestProbe();
    }
    return NSTerminateLater;
}

int risunest_probe_install(void (*callback)(void)) {
    if (![NSThread isMainThread] || !NSApp.delegate || !callback) return 0;
    Method method = class_getInstanceMethod(object_getClass(NSApp.delegate), @selector(applicationShouldTerminate:));
    if (!method) return 0;
    requestProbe = callback;
    method_setImplementation(method, (IMP)probeShouldTerminate);
    return 1;
}

void risunest_probe_begin(void) { [NSApp terminate:nil]; }

int risunest_probe_modal_mode(void) {
    return [NSThread isMainThread] && [[NSRunLoop currentRunLoop].currentMode isEqualToString:NSModalPanelRunLoopMode];
}

unsigned risunest_probe_reply_count(void) { return replies; }

int risunest_probe_reply(int approve) {
    if (![NSThread isMainThread] || !pending) return 0;
    pending = NO;
    replies++;
    [NSApp replyToApplicationShouldTerminate:approve != 0];
    return 1;
}
