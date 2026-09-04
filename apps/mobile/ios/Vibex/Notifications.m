#import <UserNotifications/UserNotifications.h>
#import "Headers/vibex_mobile.h"

@interface VibexNotificationDelegate : NSObject <UNUserNotificationCenterDelegate>
@end

@implementation VibexNotificationDelegate

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
       willPresentNotification:(UNNotification *)notification
         withCompletionHandler:(void (^)(UNNotificationPresentationOptions options))completionHandler {
    completionHandler(UNNotificationPresentationOptionBanner | UNNotificationPresentationOptionSound);
}

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
didReceiveNotificationResponse:(UNNotificationResponse *)response
         withCompletionHandler:(void (^)(void))completionHandler {
    NSString *notificationId = response.notification.request.content.userInfo[@"notificationId"];
    NSString *opaqueLocator = response.notification.request.content.userInfo[@"opaqueLocator"];
    if ([notificationId isKindOfClass:[NSString class]] && notificationId.length > 0 &&
        [opaqueLocator isKindOfClass:[NSString class]] && opaqueLocator.length > 0) {
        vibex_mobile_notification_activated(notificationId.UTF8String, opaqueLocator.UTF8String);
    }
    completionHandler();
}

@end

static VibexNotificationDelegate *vibexNotificationDelegate(void) {
    static VibexNotificationDelegate *delegate;
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        delegate = [[VibexNotificationDelegate alloc] init];
    });
    return delegate;
}

void vibex_ios_initialize_notifications(void) {
    [UNUserNotificationCenter currentNotificationCenter].delegate = vibexNotificationDelegate();
}

void vibex_ios_request_notification_authorization(void) {
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    vibex_ios_initialize_notifications();
    [center requestAuthorizationWithOptions:(UNAuthorizationOptionAlert | UNAuthorizationOptionSound)
                          completionHandler:^(BOOL granted, NSError *error) {}];
}

void vibex_ios_show_agent_notification(
    const char *notificationId,
    const char *title,
    const char *body,
    const char *opaqueLocator
) {
    if (notificationId == NULL || title == NULL || body == NULL || opaqueLocator == NULL) {
        return;
    }
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    vibex_ios_initialize_notifications();
    UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
    content.title = [NSString stringWithUTF8String:title];
    content.body = [NSString stringWithUTF8String:body];
    content.sound = [UNNotificationSound defaultSound];
    content.userInfo = @{
        @"notificationId": [NSString stringWithUTF8String:notificationId],
        @"opaqueLocator": [NSString stringWithUTF8String:opaqueLocator]
    };
    NSString *identifier = [NSString stringWithUTF8String:notificationId];
    UNNotificationRequest *request = [UNNotificationRequest requestWithIdentifier:identifier
                                                                           content:content
                                                                           trigger:nil];
    [center addNotificationRequest:request withCompletionHandler:^(NSError *error) {}];
}
