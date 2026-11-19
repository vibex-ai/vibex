package com.longbridge.nsh;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;
import android.net.nsd.NsdManager;
import android.net.nsd.NsdServiceInfo;
import android.os.Build;

import org.json.JSONObject;

import java.net.Inet4Address;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.Map;

/**
 * Local-network pairing discovery with Android's NSD, emitting the same JSON
 * the GPUI host did: the app parses the TXT record into a zero-config
 * candidate. Android 13+ asks for NEARBY_WIFI_DEVICES first.
 */
final class LanDiscovery {
    static final int PERMISSION_REQUEST = 201;
    private static final String SERVICE_TYPE = "_vibex._tcp.";

    private final Activity activity;
    private final NshHostSink sink;
    private NsdManager nsd;
    private NsdManager.DiscoveryListener listener;
    private final Map<String, NsdManager.ResolveListener> pending = new HashMap<>();

    interface NshHostSink {
        void event(String json);
    }

    LanDiscovery(Activity activity, NshHostSink sink) {
        this.activity = activity;
        this.sink = sink;
    }

    void start() {
        if (Build.VERSION.SDK_INT >= 33
                && activity.checkSelfPermission(Manifest.permission.NEARBY_WIFI_DEVICES)
                        != PackageManager.PERMISSION_GRANTED) {
            activity.requestPermissions(
                    new String[] {Manifest.permission.NEARBY_WIFI_DEVICES}, PERMISSION_REQUEST);
            return;
        }
        startNow();
    }

    void stop() {
        if (nsd != null && listener != null) {
            try {
                nsd.stopServiceDiscovery(listener);
            } catch (RuntimeException ignored) {
                // Already stopped by the system.
            }
        }
        pending.clear();
        listener = null;
        nsd = null;
    }

    void onRequestPermissionsResult(int requestCode, int[] results) {
        if (requestCode != PERMISSION_REQUEST) {
            return;
        }
        if (results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
            startNow();
        } else {
            emit("permission_denied", null, null);
        }
    }

    private void startNow() {
        stop();
        nsd = (NsdManager) activity.getSystemService(Context.NSD_SERVICE);
        listener = new NsdManager.DiscoveryListener() {
            @Override
            public void onDiscoveryStarted(String type) {}

            @Override
            public void onServiceFound(NsdServiceInfo info) {
                String type = info.getServiceType();
                if (!SERVICE_TYPE.equals(type) && !"_vibex._tcp".equals(type)) {
                    return;
                }
                resolve(info);
            }

            @Override
            public void onServiceLost(NsdServiceInfo info) {
                emit("removed", info, null);
            }

            @Override
            public void onDiscoveryStopped(String type) {}

            @Override
            public void onStartDiscoveryFailed(String type, int error) {
                emit("failed", null, null);
                stop();
            }

            @Override
            public void onStopDiscoveryFailed(String type, int error) {}
        };
        try {
            nsd.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, listener);
        } catch (RuntimeException error) {
            emit("failed", null, null);
            stop();
        }
    }

    @SuppressWarnings("deprecation")
    private void resolve(NsdServiceInfo info) {
        String key = info.getServiceName();
        if (pending.containsKey(key)) {
            return;
        }
        NsdManager.ResolveListener resolver = new NsdManager.ResolveListener() {
            @Override
            public void onResolveFailed(NsdServiceInfo failed, int error) {
                pending.remove(key);
            }

            @Override
            public void onServiceResolved(NsdServiceInfo resolved) {
                pending.remove(key);
                emit("candidate", resolved, resolved.getAttributes());
            }
        };
        pending.put(key, resolver);
        try {
            nsd.resolveService(info, resolver);
        } catch (RuntimeException error) {
            pending.remove(key);
        }
    }

    private void emit(String kind, NsdServiceInfo service, Map<String, byte[]> attributes) {
        try {
            JSONObject event = new JSONObject();
            event.put("kind", kind);
            event.put("serviceInstance", service == null ? "" : service.getServiceName());
            event.put("port", service == null ? 0 : service.getPort());
            event.put("interfaceScope", "");
            event.put("host", numericHost(service));
            JSONObject txt = new JSONObject();
            if (attributes != null) {
                for (Map.Entry<String, byte[]> entry : attributes.entrySet()) {
                    txt.put(entry.getKey(), new String(entry.getValue(), StandardCharsets.UTF_8));
                }
            }
            event.put("txt", txt);
            sink.event(event.toString());
        } catch (Exception ignored) {
            sink.event("{\"kind\":\"failed\"}");
        }
    }

    @SuppressWarnings("deprecation")
    private static String numericHost(NsdServiceInfo service) {
        if (service == null) {
            return "";
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
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
