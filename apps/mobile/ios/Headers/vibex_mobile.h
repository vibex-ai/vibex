#ifndef VIBEX_MOBILE_H
#define VIBEX_MOBILE_H

#ifdef __cplusplus
extern "C" {
#endif

void vibex_mobile_main(void);
void vibex_ios_initialize_notifications(void);
void vibex_mobile_notification_activated(
    const char *notification_id,
    const char *opaque_locator
);

void vibex_mobile_pairing_qr_scanned(const char *value);

void vibex_mobile_lan_discovery_event(const char *value);

#ifdef __cplusplus
}
#endif

#endif
