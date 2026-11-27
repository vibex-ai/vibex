package com.longbridge.nsh;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;
import android.net.nsd.NsdManager;
import android.net.nsd.NsdServiceInfo;
import android.os.Build;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONObject;

import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.NetworkInterface;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;
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
    private final HostSink sink;
    private NsdManager nsd;
    private NsdManager.DiscoveryListener listener;
    private final Map<String, NsdManager.ResolveListener> pending = new HashMap<>();

    interface HostSink {
        void event(String json);
    }

    LanDiscovery(Activity activity, HostSink sink) {
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
        resolve(info, 0);
    }

    /**
     * Resolves one found service.
     *
     * Android can report a resolution before the record's addresses are in
     * hand; a candidate without an address is one the app cannot dial, so the
     * record is resolved again rather than shown as an unusable row.
     */
    @SuppressWarnings("deprecation")
    private void resolve(NsdServiceInfo info, int attempt) {
        String key = info.getServiceName();
        if (attempt == 0 && pending.containsKey(key)) {
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
                if (numericHosts(resolved).isEmpty() && attempt < 2) {
                    new android.os.Handler(activity.getMainLooper())
                            .postDelayed(
                                    new Runnable() {
                                        @Override
                                        public void run() {
                                            resolve(info, attempt + 1);
                                        }
                                    },
                                    800);
                    return;
                }
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
            // A runtime publishes every address of every interface it has, so
            // the record is a list: the app tries them in order instead of
            // dialling one arbitrary address (a container bridge or a VPN) and
            // waiting on it.
            List<String> hosts = numericHosts(service);
            Log.i(
                    "VibexNsd",
                    "emit " + kind
                            + " instance=" + (service == null ? "" : service.getServiceName())
                            + " port=" + (service == null ? 0 : service.getPort())
                            + " hosts=" + hosts);
            JSONArray hostArray = new JSONArray();
            for (String host : hosts) {
                hostArray.put(host);
            }
            event.put("hosts", hostArray);
            event.put("host", hosts.isEmpty() ? "" : hosts.get(0));
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
    private static List<String> numericHosts(NsdServiceInfo service) {
        List<String> hosts = new ArrayList<>();
        if (service == null) {
            return hosts;
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            for (InetAddress address : service.getHostAddresses()) {
                if (address instanceof Inet4Address) {
                    hosts.add(address.getHostAddress());
                }
            }
        }
        if (hosts.isEmpty()) {
            InetAddress address = service.getHost();
            if (address instanceof Inet4Address) {
                hosts.add(address.getHostAddress());
            }
        }
        List<String> localPrefixes = localPrefixes();
        Collections.sort(hosts, (left, right) ->
                Integer.compare(rank(left, localPrefixes), rank(right, localPrefixes)));
        return hosts;
    }

    /** The /24 prefixes this phone has an address in, so its own LAN comes first. */
    private static List<String> localPrefixes() {
        List<String> prefixes = new ArrayList<>();
        try {
            for (NetworkInterface network : Collections.list(NetworkInterface.getNetworkInterfaces())) {
                for (InetAddress address : Collections.list(network.getInetAddresses())) {
                    if (!(address instanceof Inet4Address) || address.isLoopbackAddress()) {
                        continue;
                    }
                    String text = address.getHostAddress();
                    int dot = text.lastIndexOf('.');
                    if (dot > 0) {
                        prefixes.add(text.substring(0, dot + 1));
                    }
                }
            }
        } catch (Exception ignored) {
            // Without the interfaces the order falls back to the private ranges.
        }
        return prefixes;
    }

    private static int rank(String address, List<String> localPrefixes) {
        for (String prefix : localPrefixes) {
            if (address.startsWith(prefix)) {
                return 0;
            }
        }
        if (address.startsWith("192.168.")) {
            return 1;
        }
        if (address.startsWith("10.")) {
            return 2;
        }
        if (address.startsWith("172.")) {
            return 3;
        }
        if (address.startsWith("169.254.")) {
            return 5;
        }
        return 4;
    }
}
