#import <AppKit/AppKit.h>
#import <CoreFoundation/CoreFoundation.h>
#import <objc/runtime.h>

static void (*requestProbe)(void);
static void (*diagnosticProbe)(int);
static BOOL pending;
static unsigned replies;
static unsigned beginDepth;
static unsigned delegateDepth;

enum ProbeDiagnostic { ProbeBeginEnter = 1, ProbeBeginReturn, ProbeDelegateEnter, ProbeDelegateLater };

static NSApplicationTerminateReply probeShouldTerminate(id delegate, SEL selector, NSApplication *sender) {
    delegateDepth++;
    diagnosticProbe(ProbeDelegateEnter);
    if (!pending) {
        pending = YES;
        requestProbe();
    }
    diagnosticProbe(ProbeDelegateLater);
    delegateDepth--;
    return NSTerminateLater;
}

int risunest_probe_install(void (*callback)(void), void (*diagnostic)(int)) {
    if (![NSThread isMainThread] || !NSApp.delegate || !callback || !diagnostic) return 0;
    Method method = class_getInstanceMethod(object_getClass(NSApp.delegate), @selector(applicationShouldTerminate:));
    if (!method) return 0;
    requestProbe = callback;
    diagnosticProbe = diagnostic;
    method_setImplementation(method, (IMP)probeShouldTerminate);
    return 1;
}

void risunest_probe_begin(void) {
    beginDepth++;
    diagnosticProbe(ProbeBeginEnter);
    [NSApp terminate:nil];
    diagnosticProbe(ProbeBeginReturn);
    beginDepth--;
}

int risunest_probe_queue_begin(void) {
    if (![NSThread isMainThread] || pending) return 0;
    CFRunLoopRef loop = CFRunLoopGetMain();
    // Native termination must start outside Tao's event callback.
    CFRunLoopPerformBlock(loop, kCFRunLoopCommonModes, ^{
        risunest_probe_begin();
    });
    CFRunLoopWakeUp(loop);
    return 1;
}

int risunest_probe_modal_mode(void) {
    return [NSThread isMainThread] && [[NSRunLoop currentRunLoop].currentMode isEqualToString:NSModalPanelRunLoopMode];
}

unsigned risunest_probe_reply_count(void) { return replies; }
unsigned risunest_probe_begin_depth(void) { return beginDepth; }
unsigned risunest_probe_delegate_depth(void) { return delegateDepth; }

int risunest_probe_reply(int approve) {
    if (![NSThread isMainThread] || !pending) return 0;
    pending = NO;
    replies++;
    [NSApp replyToApplicationShouldTerminate:approve != 0];
    return 1;
}

static void (*productReplyObserver)(int, int, int);
static IMP productReplyOriginal;
static void (*productDecisionObserver)(int);
static IMP productDecisionOriginal;
static BOOL productBeginPending;

static void observeProductReply(id application, SEL selector, BOOL approve) {
    productReplyObserver(approve != 0, [NSThread isMainThread], risunest_probe_modal_mode());
    ((void (*)(id, SEL, BOOL))productReplyOriginal)(application, selector, approve);
}

static NSApplicationTerminateReply observeProductDecision(id delegate, SEL selector, NSApplication *sender) {
    NSApplicationTerminateReply reply =
        ((NSApplicationTerminateReply (*)(id, SEL, NSApplication *))productDecisionOriginal)(delegate, selector, sender);
    productDecisionObserver((int)reply);
    return reply;
}

int risunest_bench_queue_native_quit(void (*observer)(int, int, int), void (*decision)(int)) {
    if (![NSThread isMainThread] || !NSApp.delegate || !observer || !decision || productBeginPending) return 0;
    CFRunLoopRef loop = CFRunLoopGetMain();
    if (!loop) return 0;
    if (!productReplyOriginal) {
        Method should = class_getInstanceMethod(object_getClass(NSApp.delegate), @selector(applicationShouldTerminate:));
        if (!should) return 0;
        Method method = class_getInstanceMethod(object_getClass(NSApp), @selector(replyToApplicationShouldTerminate:));
        if (!method) return 0;
        productReplyObserver = observer;
        productDecisionObserver = decision;
        productReplyOriginal = method_setImplementation(method, (IMP)observeProductReply);
        productDecisionOriginal = method_setImplementation(should, (IMP)observeProductDecision);
    } else if (productReplyObserver != observer || productDecisionObserver != decision) {
        return 0;
    }
    productBeginPending = YES;
    CFRunLoopPerformBlock(loop, kCFRunLoopCommonModes, ^{
        [NSApp terminate:nil];
        productBeginPending = NO;
    });
    CFRunLoopWakeUp(loop);
    return 1;
}

int risunest_bench_queue_repeated_quit(void) {
    if (![NSThread isMainThread] || !productReplyOriginal) return 0;
    CFRunLoopRef loop = CFRunLoopGetMain();
    if (!loop) return 0;
    // The first quit is still waiting in AppKit's modal loop, as a repeated Command-Q would find it.
    NSArray *modes = @[(__bridge NSString *)kCFRunLoopCommonModes, NSModalPanelRunLoopMode];
    CFRunLoopPerformBlock(loop, (__bridge CFTypeRef)modes, ^{
        [NSApp terminate:nil];
    });
    CFRunLoopWakeUp(loop);
    return 1;
}
