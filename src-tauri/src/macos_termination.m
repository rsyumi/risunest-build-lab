#import <AppKit/AppKit.h>
#import <CoreFoundation/CoreFoundation.h>
#import <CoreServices/CoreServices.h>
#import <objc/runtime.h>

static int (*requestQuit)(int sessionEnding);
static BOOL pending;

// Logout, restart and shutdown send the quit event with a reason; Cmd+Q and the Dock send none.
static int sessionEnding(void) {
    NSAppleEventDescriptor *event = [[NSAppleEventManager sharedAppleEventManager] currentAppleEvent];
    OSType reason = [[event attributeDescriptorForKeyword:kAEQuitReason] typeCodeValue];
    switch (reason) {
        case kAELogOut:
        case kAEReallyLogOut:
        case kAEShowRestartDialog:
        case kAEShowShutdownDialog:
        case kAERestart:
        case kAEShutDown:
            return 1;
        default:
            return 0;
    }
}

static NSApplicationTerminateReply shouldTerminate(id delegate, SEL command, NSApplication *sender) {
    if (pending) {
        // A repeated quit ends the app when the document left the pending one unanswered.
        if (!requestQuit || requestQuit(sessionEnding()) != 0) return NSTerminateLater;
        pending = NO;
        return NSTerminateNow;
    }
    pending = YES;
    int disposition = requestQuit ? requestQuit(sessionEnding()) : 0;
    if (disposition > 0) return NSTerminateLater;
    pending = NO;
    return disposition < 0 ? NSTerminateCancel : NSTerminateNow;
}

int risunest_reply_termination(int approve) {
    if (![NSThread isMainThread] || !pending) return 0;
    pending = NO;
    [NSApp replyToApplicationShouldTerminate:approve != 0];
    return 1;
}

int risunest_termination_pending(void) {
    return [NSThread isMainThread] && pending;
}

int risunest_queue_termination_response(void (*callback)(void *), void *context) {
    if (!callback || !context) return 0;
    CFRunLoopRef loop = CFRunLoopGetMain();
    if (!loop) return 0;
    // A native YES reply must run outside Tao's event callback.
    CFRunLoopPerformBlock(loop, kCFRunLoopCommonModes, ^{
        callback(context);
    });
    CFRunLoopWakeUp(loop);
    return 1;
}

int risunest_install_termination_handler(int (*callback)(int sessionEnding)) {
    if (![NSThread isMainThread] || !NSApp.delegate || !callback) return 0;
    Class delegateClass = object_getClass(NSApp.delegate);
    SEL selector = @selector(applicationShouldTerminate:);
    // Never replace a runtime implementation introduced by a dependency update.
    if (class_getInstanceMethod(delegateClass, selector)) return 0;
    struct objc_method_description method = protocol_getMethodDescription(
        @protocol(NSApplicationDelegate), selector, NO, YES);
    if (!method.types) return 0;
    requestQuit = callback;
    if (!class_addMethod(delegateClass, selector, (IMP)shouldTerminate, method.types)) {
        requestQuit = NULL;
        return 0;
    }
    return 1;
}
