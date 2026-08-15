#import <Foundation/Foundation.h>
#import "vibex_mobile.h"

@interface VibexLanDiscoveryController : NSObject <NSNetServiceBrowserDelegate, NSNetServiceDelegate>
@property(nonatomic, strong) NSNetServiceBrowser *browser;
@property(nonatomic, strong) NSMutableDictionary<NSString *, NSNetService *> *services;
@end

static void VibexEmitLanDiscoveryEvent(NSDictionary *event) {
    NSError *error = nil;
    NSData *data = [NSJSONSerialization dataWithJSONObject:event options:0 error:&error];
    if (data == nil || error != nil) {
        vibex_mobile_lan_discovery_event("{\"kind\":\"failed\"}");
        return;
    }
    NSString *json = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    vibex_mobile_lan_discovery_event(json.UTF8String);
}

@implementation VibexLanDiscoveryController

- (instancetype)init {
    self = [super init];
    if (self) {
        _services = [NSMutableDictionary dictionary];
    }
    return self;
}

- (void)start {
    [self stop];
    self.browser = [[NSNetServiceBrowser alloc] init];
    self.browser.delegate = self;
    [self.browser searchForServicesOfType:@"_vibex._tcp." inDomain:@"local."];
}

- (void)stop {
    for (NSNetService *service in self.services.allValues) {
        service.delegate = nil;
        [service stop];
    }
    [self.services removeAllObjects];
    [self.browser stop];
    self.browser.delegate = nil;
    self.browser = nil;
}

- (void)netServiceBrowser:(NSNetServiceBrowser *)browser
           didFindService:(NSNetService *)service
               moreComing:(BOOL)moreComing {
    self.services[service.name] = service;
    service.delegate = self;
    [service resolveWithTimeout:5.0];
}

- (void)netServiceBrowser:(NSNetServiceBrowser *)browser
         didRemoveService:(NSNetService *)service
               moreComing:(BOOL)moreComing {
    [self.services removeObjectForKey:service.name];
    VibexEmitLanDiscoveryEvent(@{
        @"kind": @"removed",
        @"serviceInstance": service.name ?: @"",
        @"host": @"",
        @"port": @0,
        @"interfaceScope": service.domain ?: @"",
        @"txt": @{},
    });
}

- (void)netServiceBrowser:(NSNetServiceBrowser *)browser
             didNotSearch:(NSDictionary<NSString *, NSNumber *> *)errorDict {
    NSNumber *code = errorDict[NSNetServicesErrorCode];
    NSString *kind = code.integerValue == NSNetServicesMissingRequiredConfigurationError
        ? @"permission_denied"
        : @"failed";
    VibexEmitLanDiscoveryEvent(@{@"kind": kind});
}

- (void)netServiceDidResolveAddress:(NSNetService *)sender {
    NSDictionary<NSString *, NSData *> *raw = sender.TXTRecordData == nil
        ? @{}
        : [NSNetService dictionaryFromTXTRecordData:sender.TXTRecordData];
    NSMutableDictionary<NSString *, NSString *> *txt = [NSMutableDictionary dictionary];
    [raw enumerateKeysAndObjectsUsingBlock:^(NSString *key, NSData *value, BOOL *stop) {
        NSString *decoded = [[NSString alloc] initWithData:value encoding:NSUTF8StringEncoding];
        if (decoded != nil) {
            txt[key] = decoded;
        }
    }];
    VibexEmitLanDiscoveryEvent(@{
        @"kind": @"candidate",
        @"serviceInstance": sender.name ?: @"",
        @"host": sender.hostName ?: @"",
        @"port": @(sender.port),
        @"interfaceScope": sender.domain ?: @"",
        @"txt": txt,
    });
}

- (void)netService:(NSNetService *)sender didNotResolve:(NSDictionary<NSString *, NSNumber *> *)errorDict {
    [self.services removeObjectForKey:sender.name];
}

@end

static VibexLanDiscoveryController *VibexLanDiscovery;

void vibex_ios_start_lan_discovery(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (VibexLanDiscovery == nil) {
            VibexLanDiscovery = [[VibexLanDiscoveryController alloc] init];
        }
        [VibexLanDiscovery start];
    });
}

void vibex_ios_stop_lan_discovery(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [VibexLanDiscovery stop];
    });
}
