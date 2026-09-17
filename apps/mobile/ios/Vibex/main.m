// Vibex iOS host.
//
// gpui-pre-mobile does not enter UIApplicationMain itself: the host owns the
// UIKit app delegate and forwards everything GPUI needs — the launch callback,
// the frame clock, and the app lifecycle. The GPUI window is created by
// `gpui_ios_run_demo()` (via the callback Vibex registers) and made key and
// visible by the platform, so this file never builds a window of its own.

#import <UIKit/UIKit.h>
#import <QuartzCore/QuartzCore.h>
#import "vibex_mobile.h"

// gpui-pre-mobile C ABI (see longbridge/gpui-mobile, src/ios/ffi.rs).
extern void gpui_ios_run_demo(void);
extern void *gpui_ios_get_window(void);
extern bool gpui_ios_request_frame(void *window);
extern void gpui_ios_set_frame_waker(void *window, void (*waker)(void *), void *context);
extern void gpui_ios_will_enter_foreground(void *app);
extern void gpui_ios_did_become_active(void *app);
extern void gpui_ios_will_resign_active(void *app);
extern void gpui_ios_did_enter_background(void *app);
extern void gpui_ios_will_terminate(void *app);
extern void gpui_ios_handle_open_url(void *url);

@interface VibexAppDelegate : UIResponder <UIApplicationDelegate>
@property (nonatomic, assign) void *gpuiWindow;
@property (nonatomic, strong) CADisplayLink *displayLink;
@end

@implementation VibexAppDelegate

- (BOOL)application:(UIApplication *)application
    didFinishLaunchingWithOptions:(NSDictionary<UIApplicationLaunchOptionsKey, id> *)launchOptions {
    vibex_ios_initialize_notifications();

    // Register the root view before starting GPUI: the run loop invokes the
    // callback once, and that is where the window is created.
    vibex_mobile_register_app();
    gpui_ios_run_demo();

    self.gpuiWindow = gpui_ios_get_window();
    if (self.gpuiWindow) {
        [self startFrameClock];
    } else {
        NSLog(@"Vibex: GPUI did not create a window");
    }
    return YES;
}

// GPUI reports after every frame whether it wants another one; the link is
// paused in between and resumed by the waker, so an idle screen costs no CPU.
- (void)startFrameClock {
    self.displayLink = [CADisplayLink displayLinkWithTarget:self selector:@selector(renderFrame)];
    [self.displayLink addToRunLoop:[NSRunLoop mainRunLoop] forMode:NSRunLoopCommonModes];
    gpui_ios_set_frame_waker(self.gpuiWindow, VibexResumeFrames, (__bridge void *)self);
}

- (void)stopFrameClock {
    [self.displayLink invalidate];
    self.displayLink = nil;
}

- (void)renderFrame {
    if (self.gpuiWindow && !gpui_ios_request_frame(self.gpuiWindow)) {
        self.displayLink.paused = YES;
    }
}

static void VibexResumeFrames(void *context) {
    VibexAppDelegate *delegate = (__bridge VibexAppDelegate *)context;
    delegate.displayLink.paused = NO;
}

- (void)applicationWillEnterForeground:(UIApplication *)application {
    gpui_ios_will_enter_foreground(self.gpuiWindow);
    // Vibex suspends its remote connection while backgrounded; this is what
    // brings the UI back and reconnects.
    vibex_mobile_set_lifecycle(1);
    if (!self.displayLink && self.gpuiWindow) {
        [self startFrameClock];
    }
}

- (void)applicationDidBecomeActive:(UIApplication *)application {
    gpui_ios_did_become_active(self.gpuiWindow);
}

- (void)applicationWillResignActive:(UIApplication *)application {
    gpui_ios_will_resign_active(self.gpuiWindow);
}

- (void)applicationDidEnterBackground:(UIApplication *)application {
    gpui_ios_did_enter_background(self.gpuiWindow);
    vibex_mobile_set_lifecycle(0);
    [self stopFrameClock];
}

- (BOOL)application:(UIApplication *)application
            openURL:(NSURL *)url
            options:(NSDictionary<UIApplicationOpenURLOptionsKey, id> *)options {
    NSString *urlString = [url absoluteString];
    gpui_ios_handle_open_url((__bridge void *)urlString);
    return YES;
}

- (void)applicationWillTerminate:(UIApplication *)application {
    [self stopFrameClock];
    gpui_ios_will_terminate(self.gpuiWindow);
}

@end

int main(int argc, char *argv[]) {
    @autoreleasepool {
        return UIApplicationMain(argc, argv, nil, NSStringFromClass([VibexAppDelegate class]));
    }
}
