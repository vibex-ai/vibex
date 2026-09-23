package ai.vibex.mobile;

import android.Manifest;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.ActivityNotFoundException;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.net.nsd.NsdManager;
import android.net.nsd.NsdServiceInfo;
import android.os.Build;
import android.os.Bundle;
import android.os.PowerManager;
import android.provider.Settings;

import androidx.core.view.WindowCompat;

import org.json.JSONObject;

import java.net.Inet4Address;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.Map;

/**
 * Vibex's Android host.
 *
 * Extends the vendored {@code dev.gpui.mobile.GpuiInputActivity}: gpui-pre-mobile
 * calls {@code gpuiShowKeyboard}/{@code gpuiHideKeyboard}/{@code
 * gpuiResetComposition} on the running activity and ships the matching
 * {@code InputConnection} bridge, so the IME host must stay that exact class.
 * Everything Vibex-specific — notifications, LAN discovery, the foreground
 * service, lifecycle reporting — lives here.
 */
public final class GpuiNativeActivity extends dev.gpui.mobile.GpuiInputActivity {
    private static final int LOCAL_NETWORK_PERMISSION_REQUEST = 4102;
    private static final int NOTIFICATION_PERMISSION_REQUEST = 4103;
    private static final String AGENT_NOTIFICATION_CHANNEL = "agent_activity";
    private static final String EXTRA_NOTIFICATION_ID = "vibex.notification.id";
    private static final String EXTRA_NOTIFICATION_LOCATOR = "vibex.notification.locator";
    private static final String NOTIFICATION_PERMISSION_PREFERENCES = "vibex.notifications";
    private static final String NOTIFICATION_PERMISSION_REQUESTED = "permission_requested";
    private static final String VIBEX_SERVICE_TYPE = "_vibex._tcp.";

    static {
        // NativeActivity's manifest loader does not register the library with
        // this ClassLoader, so Java-declared JNI callbacks need an explicit load.
        System.loadLibrary("vibex_mobile");
    }

    private NsdManager nsdManager;
    private NsdManager.DiscoveryListener lanDiscoveryListener;
    private final Map<String, NsdManager.ResolveListener> pendingResolutions = new HashMap<>();

    private static native void nativeOnLanDiscoveryEvent(String payload);
    private static native void nativeOnAppLifecycle(boolean foreground);
    private static native void nativeOnNotificationActivated(
            String notificationId, String opaqueLocator);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        // Keep the GPUI content rectangle below Android system bars; the
        // platform republishes that geometry as the window's safe-area insets.
        WindowCompat.setDecorFitsSystemWindows(getWindow(), true);

        createAgentNotificationChannel();
        handleNotificationIntent(getIntent());
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        handleNotificationIntent(intent);
    }

    public void requestNotificationAuthorization() {
        runOnUiThread(() -> {
            if (Build.VERSION.SDK_INT >= 33
                    && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                            != PackageManager.PERMISSION_GRANTED
                    && !getSharedPreferences(NOTIFICATION_PERMISSION_PREFERENCES, MODE_PRIVATE)
                            .getBoolean(NOTIFICATION_PERMISSION_REQUESTED, false)) {
                getSharedPreferences(NOTIFICATION_PERMISSION_PREFERENCES, MODE_PRIVATE)
                        .edit()
                        .putBoolean(NOTIFICATION_PERMISSION_REQUESTED, true)
                        .apply();
                requestPermissions(
                        new String[] {Manifest.permission.POST_NOTIFICATIONS},
                        NOTIFICATION_PERMISSION_REQUEST);
            }
        });
    }

    public void startRemoteConnectionService() {
        RemoteConnectionService.start(getApplicationContext());
    }

    public void stopRemoteConnectionService() {
        RemoteConnectionService.stop(getApplicationContext());
    }

    /** Whether the system exempts this app from Doze/App Standby battery optimizations. */
    public boolean isIgnoringBatteryOptimizations() {
        PowerManager powerManager = getSystemService(PowerManager.class);
        return powerManager != null
                && powerManager.isIgnoringBatteryOptimizations(getPackageName());
    }

    /**
     * Opens the system dialog that asks the user to exempt the app from battery
     * optimizations, falling back to the app details page when unavailable.
     */
    public void requestIgnoreBatteryOptimizations() {
        runOnUiThread(() -> {
            Intent intent = new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS)
                    .setData(Uri.parse("package:" + getPackageName()))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            try {
                startActivity(intent);
            } catch (ActivityNotFoundException error) {
                Intent appDetails = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS)
                        .setData(Uri.parse("package:" + getPackageName()))
                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                try {
                    startActivity(appDetails);
                } catch (ActivityNotFoundException ignored) {
                    // Nothing else to open; the user can find the page manually.
                }
            }
        });
    }

    public void showAgentNotification(
            String notificationId, String title, String body, String opaqueLocator) {
        runOnUiThread(() -> {
            Intent intent = new Intent(this, GpuiNativeActivity.class)
                    .setAction("ai.vibex.mobile.OPEN_AGENT_NOTIFICATION")
                    .putExtra(EXTRA_NOTIFICATION_ID, notificationId)
                    .putExtra(EXTRA_NOTIFICATION_LOCATOR, opaqueLocator)
                    .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP);
            int requestCode = notificationId.hashCode() & 0x7fffffff;
            PendingIntent pendingIntent = PendingIntent.getActivity(
                    this,
                    requestCode,
                    intent,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
            Notification notification = new Notification.Builder(this, AGENT_NOTIFICATION_CHANNEL)
                    .setSmallIcon(ai.vibex.mobile.R.drawable.ic_launcher_foreground)
                    .setContentTitle(title)
                    .setContentText(body)
                    .setCategory(Notification.CATEGORY_MESSAGE)
                    .setAutoCancel(true)
                    .setContentIntent(pendingIntent)
                    .build();
            NotificationManager manager = getSystemService(NotificationManager.class);
            manager.notify(notificationId, 0, notification);
        });
    }

    private void createAgentNotificationChannel() {
        NotificationManager manager = getSystemService(NotificationManager.class);
        NotificationChannel channel = new NotificationChannel(
                AGENT_NOTIFICATION_CHANNEL,
                "Agent activity",
                NotificationManager.IMPORTANCE_DEFAULT);
        channel.setDescription("Agent approvals, input requests, and completed work");
        manager.createNotificationChannel(channel);
    }

    private static void handleNotificationIntent(Intent intent) {
        if (intent == null) {
            return;
        }
        String notificationId = intent.getStringExtra(EXTRA_NOTIFICATION_ID);
        String opaqueLocator = intent.getStringExtra(EXTRA_NOTIFICATION_LOCATOR);
        if (notificationId != null && !notificationId.isEmpty()
                && opaqueLocator != null && !opaqueLocator.isEmpty()) {
            intent.removeExtra(EXTRA_NOTIFICATION_ID);
            intent.removeExtra(EXTRA_NOTIFICATION_LOCATOR);
            nativeOnNotificationActivated(notificationId, opaqueLocator);
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        nativeOnAppLifecycle(true);
    }

    @Override
    protected void onPause() {
        nativeOnAppLifecycle(false);
        super.onPause();
    }

    public void launchPairingQrScanner() {
        runOnUiThread(() ->
                startActivity(new Intent(this, PairingQrScannerActivity.class)));
    }

    /**
     * Opens this app's page in the system settings.
     *
     * A denied local-network permission can only be re-granted there, and the
     * pairing screen that reports the denial has no other way to send the user
     * to it. A missing settings app is swallowed: the caller already explained
     * the problem, and there is nothing else the user could do from here.
     */
    public void openAppSettings() {
        runOnUiThread(() -> {
            Intent intent = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS);
            intent.setData(Uri.parse("package:" + getPackageName()));
            try {
                startActivity(intent);
            } catch (ActivityNotFoundException ignored) {
                // No settings app to open.
            }
        });
    }

    public void startLanPairingDiscovery() {
        runOnUiThread(() -> {
            if (Build.VERSION.SDK_INT >= 33
                    && checkSelfPermission(Manifest.permission.NEARBY_WIFI_DEVICES)
                            != PackageManager.PERMISSION_GRANTED) {
                requestPermissions(
                        new String[] {Manifest.permission.NEARBY_WIFI_DEVICES},
                        LOCAL_NETWORK_PERMISSION_REQUEST);
                return;
            }
            startLanPairingDiscoveryAfterPermission();
        });
    }

    public void stopLanPairingDiscovery() {
        runOnUiThread(this::stopLanPairingDiscoveryOnUiThread);
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode != LOCAL_NETWORK_PERMISSION_REQUEST) {
            return;
        }
        if (grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
            startLanPairingDiscoveryAfterPermission();
        } else {
            emitLanDiscoveryEvent("permission_denied", null, null);
        }
    }

    private void startLanPairingDiscoveryAfterPermission() {
        stopLanPairingDiscoveryOnUiThread();
        nsdManager = (NsdManager) getSystemService(Context.NSD_SERVICE);
        lanDiscoveryListener = new NsdManager.DiscoveryListener() {
            @Override
            public void onDiscoveryStarted(String serviceType) {}

            @Override
            public void onServiceFound(NsdServiceInfo serviceInfo) {
                String type = serviceInfo.getServiceType();
                if (!VIBEX_SERVICE_TYPE.equals(type) && !"_vibex._tcp".equals(type)) {
                    return;
                }
                resolveLanService(serviceInfo);
            }

            @Override
            public void onServiceLost(NsdServiceInfo serviceInfo) {
                emitLanDiscoveryEvent("removed", serviceInfo, null);
            }

            @Override
            public void onDiscoveryStopped(String serviceType) {}

            @Override
            public void onStartDiscoveryFailed(String serviceType, int errorCode) {
                emitLanDiscoveryEvent("failed", null, null);
                stopLanPairingDiscoveryOnUiThread();
            }

            @Override
            public void onStopDiscoveryFailed(String serviceType, int errorCode) {
                lanDiscoveryListener = null;
            }
        };
        try {
            nsdManager.discoverServices(
                    VIBEX_SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, lanDiscoveryListener);
        } catch (RuntimeException error) {
            emitLanDiscoveryEvent("failed", null, null);
            stopLanPairingDiscoveryOnUiThread();
        }
    }

    @SuppressWarnings("deprecation")
    private void resolveLanService(NsdServiceInfo serviceInfo) {
        String key = serviceInfo.getServiceName();
        if (pendingResolutions.containsKey(key)) {
            return;
        }
        NsdManager.ResolveListener listener = new NsdManager.ResolveListener() {
            @Override
            public void onResolveFailed(NsdServiceInfo failed, int errorCode) {
                pendingResolutions.remove(key);
            }

            @Override
            public void onServiceResolved(NsdServiceInfo resolved) {
                pendingResolutions.remove(key);
                emitLanDiscoveryEvent("candidate", resolved, resolved.getAttributes());
            }
        };
        pendingResolutions.put(key, listener);
        try {
            nsdManager.resolveService(serviceInfo, listener);
        } catch (RuntimeException error) {
            pendingResolutions.remove(key);
        }
    }

    private void stopLanPairingDiscoveryOnUiThread() {
        if (nsdManager != null && lanDiscoveryListener != null) {
            try {
                nsdManager.stopServiceDiscovery(lanDiscoveryListener);
            } catch (RuntimeException ignored) {
                // The listener may already have been stopped by Android.
            }
        }
        pendingResolutions.clear();
        lanDiscoveryListener = null;
        nsdManager = null;
    }

    private static void emitLanDiscoveryEvent(
            String kind, NsdServiceInfo service, Map<String, byte[]> attributes) {
        try {
            JSONObject event = new JSONObject();
            event.put("kind", kind);
            event.put("serviceInstance", service == null ? "" : service.getServiceName());
            event.put("port", service == null ? 0 : service.getPort());
            event.put("interfaceScope", "");
            event.put("host", resolvedNumericHost(service));
            JSONObject txt = new JSONObject();
            if (attributes != null) {
                for (Map.Entry<String, byte[]> entry : attributes.entrySet()) {
                    txt.put(entry.getKey(), new String(entry.getValue(), StandardCharsets.UTF_8));
                }
            }
            event.put("txt", txt);
            nativeOnLanDiscoveryEvent(event.toString());
        } catch (Exception ignored) {
            nativeOnLanDiscoveryEvent("{\"kind\":\"failed\"}");
        }
    }

    @SuppressWarnings("deprecation")
    private static String resolvedNumericHost(NsdServiceInfo service) {
        if (service == null) {
            return "";
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            // Zero-config listeners are IPv4-only; Rust still accepts the
            // numeric fallback for Direct HTTPS candidates.
            String fallback = "";
            for (InetAddress address : service.getHostAddresses()) {
                if (address instanceof Inet4Address) {
                    return address.getHostAddress();
                }
                if (fallback.isEmpty()) {
                    fallback = address.getHostAddress();
                }
            }
            if (!fallback.isEmpty()) {
                return fallback;
            }
        }
        InetAddress address = service.getHost();
        return address == null ? "" : address.getHostAddress();
    }
}
