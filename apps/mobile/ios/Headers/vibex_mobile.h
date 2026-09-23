#ifndef VIBEX_MOBILE_H
#define VIBEX_MOBILE_H

#ifdef __cplusplus
extern "C" {
#endif

// Registers the GPUI root-view callback; call before `gpui_ios_run_demo()`.
void vibex_mobile_register_app(void);

// Reports an app lifecycle transition: 1 for foreground, 0 for background.
void vibex_mobile_set_lifecycle(int foreground);

void vibex_ios_initialize_notifications(void);
void vibex_mobile_notification_activated(
    const char *notification_id,
    const char *opaque_locator
);

void vibex_mobile_pairing_qr_scanned(const char *value);

// Opens this app's page in the system settings, where a denied local-network
// permission can be re-granted.
void vibex_ios_open_app_settings(void);

void vibex_mobile_lan_discovery_event(const char *value);

#ifdef __cplusplus
}
#endif

#endif
