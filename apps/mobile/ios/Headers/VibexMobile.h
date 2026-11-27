//
//  VibexMobile.h
//  The C ABI between the Vibex Swift host and the Rust app in
//  `apps/mobile/src/platform/ios.rs`.
//
//  The shell owns what iOS keeps out of a Rust process: the camera, Bonjour,
//  the share intake, the quick actions and the background task. It hands what
//  it observes to the library through the `*_result` / `*_event` / `*_photo` /
//  `*_failed` / `*_text` calls, and answers what the app asks for by polling
//  `vibex_mobile_take_host_request` on its own poll loop. The contract is the
//  same one the Android shell implements in `android/`, so the Rust side has
//  one host seam rather than two.
//
//  `tests/ios_host_abi.rs` checks that every function here is exported by the
//  Rust library and that every symbol the Swift host calls is declared here.
//

#ifndef VIBEX_MOBILE_H
#define VIBEX_MOBILE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/// Mounts the app into `host` (a live `UIView` on the main thread) and
/// returns the pointer that stops it. Pass it back to
/// `vibex_mobile_detach` once.
void *vibex_mobile_attach(void *host);

/// Stops the app and removes its view.
void vibex_mobile_detach(void *app);

/// Makes NSH's notification center the app's delegate. Call before the app
/// finishes launching, so a notification press that started the process is
/// kept until the app listens.
void vibex_mobile_install_notifications(void);

/// Names the phone in the runtime's paired-device list (`UIDevice.name`).
void vibex_mobile_set_device_name(const char *value);

/// Tells the app that Scan can open the shell's camera.
void vibex_mobile_set_scanner_available(int available);

/// Tells the app that Find can use the shell's Bonjour browser.
void vibex_mobile_set_discovery_available(int available);

/// Tells the app that the composer can open the shell's camera.
void vibex_mobile_set_camera_available(int available);

/// A pairing link the scanner read.
void vibex_mobile_scan_result(const char *value);

/// One local-network discovery event, as the JSON contract describes:
/// `{"kind":"candidate","key":…,"name":…,"hosts":[…],"port":…,"txt":{…}}`,
/// `{"kind":"removed","key":…}`, `{"kind":"permission_denied"}` or
/// `{"kind":"failed","message":…}`.
void vibex_mobile_discovery_event(const char *value);

/// The path of a photo the shell's camera wrote (JPEG).
void vibex_mobile_camera_photo(const char *path);

/// Why the shell could not take a photo.
void vibex_mobile_camera_failed(const char *reason);

/// Text another app shared into Vibex.
void vibex_mobile_share_text(const char *value);

/// A quick action asked for a new session.
void vibex_mobile_new_session(void);

/// The next thing the app asks the shell to do: 0 none, 1 scan, 2 start
/// discovery, 3 stop discovery, 4 camera, 5 app settings.
int32_t vibex_mobile_take_host_request(void);

/// The pending screen-awake change: -1 none, 0 off, 1 on.
int32_t vibex_mobile_take_keep_screen_on(void);

/// The pending "stay connected in the background" change: -1 none, 0 stop,
/// 1 start.
int32_t vibex_mobile_take_connection_service(void);

#ifdef __cplusplus
}
#endif

#endif /* VIBEX_MOBILE_H */
