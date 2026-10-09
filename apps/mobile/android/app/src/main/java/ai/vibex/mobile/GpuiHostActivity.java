package ai.vibex.mobile;

import android.Manifest;
import android.app.Activity;
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
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.text.TextWatcher;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputConnectionWrapper;
import android.view.inputmethod.InputMethodManager;
import android.widget.EditText;

import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsCompat;

import org.json.JSONObject;

import java.net.Inet4Address;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.Map;

/**
 * Vibex's Android host.
 *
 * A plain {@link Activity} that owns a {@link SurfaceView} and hands everything
 * the GPUI host entry needs — the surface, touch and key events, the IME, window
 * insets and the lifecycle — to {@code vibex_mobile}'s
 * {@code Java_ai_vibex_mobile_GpuiHostActivity_*} entry points.
 *
 * <p>The native side runs a process-lived render thread, so this Activity can be
 * destroyed and recreated freely: a new instance re-attaches its surface to the
 * same GPUI window instead of trying to build a second platform. That is what
 * keeps a backgrounded app usable after Android reclaims its Activity while the
 * foreground connection service keeps the process alive.
 *
 * <p>Everything Vibex-specific — notifications, LAN discovery, the foreground
 * service, the battery allowlist — lives here too, and is reached from Rust by
 * method name through the current Activity.
 */
public final class GpuiHostActivity extends Activity {
    private static final int LOCAL_NETWORK_PERMISSION_REQUEST = 4102;
    private static final int NOTIFICATION_PERMISSION_REQUEST = 4103;
    private static final String AGENT_NOTIFICATION_CHANNEL = "agent_activity";
    private static final String EXTRA_NOTIFICATION_ID = "vibex.notification.id";
    private static final String EXTRA_NOTIFICATION_LOCATOR = "vibex.notification.locator";
    private static final String NOTIFICATION_PERMISSION_PREFERENCES = "vibex.notifications";
    private static final String NOTIFICATION_PERMISSION_REQUESTED = "permission_requested";
    private static final String VIBEX_SERVICE_TYPE = "_vibex._tcp.";

    static {
        // A plain Activity has no `android.app.lib_name` meta-data to load the
        // native library, so the host loads the JNI entry points itself.
        System.loadLibrary("vibex_mobile");
    }

    private SurfaceView surfaceView;
    private InputProxy input;

    private NsdManager nsdManager;
    private NsdManager.DiscoveryListener lanDiscoveryListener;
    private final Map<String, NsdManager.ResolveListener> pendingResolutions = new HashMap<>();

    private static native void nativeStart(Activity activity, String dataDir);
    private static native void nativeSurfaceCreated(Surface surface, float scale);
    private static native void nativeSurfaceDestroyed();
    private static native void nativeOnAppLifecycle(boolean foreground);
    private static native void nativeInsets(int left, int top, int right, int bottom);
    private static native void nativeKeyboardState(boolean visible, float height);
    private static native void nativeIme(long session, int kind, String text, int start, int end);
    private static native void nativeTouch(int action, int pointerId, float x, float y);
    private static native void nativeKey(int keyCode, int action, int metaState);
    private static native void nativeOnLanDiscoveryEvent(String payload);
    private static native void nativeOnNotificationActivated(
            String notificationId, String opaqueLocator);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        // GPUI draws edge to edge and pads its own root view by the insets
        // reported through `nativeInsets`, so the window must not inset the
        // surface the system bars leave behind.
        WindowCompat.setDecorFitsSystemWindows(getWindow(), false);

        surfaceView = new SurfaceView(this);
        surfaceView.getHolder().addCallback(new SurfaceHolder.Callback() {
            @Override
            public void surfaceCreated(SurfaceHolder holder) {
                attachSurface(holder);
            }

            @Override
            public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
                attachSurface(holder);
                reportCurrentInsets();
            }

            @Override
            public void surfaceDestroyed(SurfaceHolder holder) {
                nativeSurfaceDestroyed();
            }
        });
        setContentView(surfaceView);

        View decor = getWindow().getDecorView();
        ViewCompat.setOnApplyWindowInsetsListener(decor, (view, insets) -> {
            reportInsets(insets);
            return insets;
        });

        createAgentNotificationChannel();
        handleNotificationIntent(getIntent());
        nativeStart(this, getFilesDir().getAbsolutePath());
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        handleNotificationIntent(intent);
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

    /** Hands the surface Android just created to the render thread. */
    private void attachSurface(SurfaceHolder holder) {
        Surface surface = holder.getSurface();
        if (surface == null || !surface.isValid()) {
            return;
        }
        nativeSurfaceCreated(surface, getResources().getDisplayMetrics().density);
    }

    /** Reports the window insets GPUI has to lay out around. */
    private void reportCurrentInsets() {
        WindowInsetsCompat insets = ViewCompat.getRootWindowInsets(getWindow().getDecorView());
        if (insets != null) {
            reportInsets(insets);
        }
    }

    private void reportInsets(WindowInsetsCompat insets) {
        Insets bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars()
                        | WindowInsetsCompat.Type.displayCutout());
        // The GPUI render thread may not have processed surfaceChanged yet.
        // Send edge distances, never a rect derived from this View's new size.
        nativeInsets(bars.left, bars.top, bars.right, bars.bottom);

        boolean visible = insets.isVisible(WindowInsetsCompat.Type.ime());
        Insets ime = insets.getInsets(WindowInsetsCompat.Type.ime());
        // `adjustResize` still shrinks the window for the keyboard below API 30,
        // so the surface already excludes it there; from API 30 on the app is
        // edge to edge and this inset is the only thing that lifts the composer
        // above the keyboard. The height GPUI wants is in logical pixels.
        boolean heightInLayout = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R;
        float density = getResources().getDisplayMetrics().density;
        nativeKeyboardState(visible, visible && heightInLayout ? ime.bottom / density : 0f);
    }

    @Override
    public boolean dispatchTouchEvent(MotionEvent event) {
        int action = event.getActionMasked();
        int index = event.getActionIndex();
        if (action == MotionEvent.ACTION_POINTER_DOWN || action == MotionEvent.ACTION_POINTER_UP) {
            // `motion_event` collapses a pointer change onto the affected
            // pointer, so only that one is forwarded.
            nativeTouch(action, event.getPointerId(index), event.getX(index), event.getY(index));
            return true;
        }
        for (int i = 0; i < event.getPointerCount(); i++) {
            nativeTouch(action, event.getPointerId(i), event.getX(i), event.getY(i));
        }
        return true;
    }

    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        // While the software keyboard is up the IME proxy owns the keys: it
        // reports BACK as a dismissed IME, which is the same thing GPUI would
        // otherwise do on its own.
        if (input != null && input.hasFocus() && super.dispatchKeyEvent(event)) {
            return true;
        }
        int action = event.getAction();
        if (action == KeyEvent.ACTION_DOWN || action == KeyEvent.ACTION_UP) {
            nativeKey(
                    event.getKeyCode(),
                    action == KeyEvent.ACTION_DOWN ? 0 : 1,
                    event.getMetaState());
        }
        return true;
    }

    // ── software keyboard ────────────────────────────────────────────────────
    //
    // The InputConnection bridge below is adapted from the gpui-mobile example's
    // `dev.gpui.mobile.GpuiInputActivity` (AGPL-3.0-or-later, rev 4e4668d). It
    // used to be vendored as a `NativeActivity` base class; the host entry owns
    // its Activity instead, so the same proxy lives here and reports through the
    // `nativeIme` entry point for this class.

    /**
     * Shows the UI-thread InputConnection proxy the IME writes into.
     *
     * A {@code NativeActivity} key-event connection cannot compose text or
     * delete surrounding text; the proxy's {@link InputConnectionWrapper} turns
     * every IME mutation into a {@code nativeIme} callback that GPUI applies to
     * the focused input.
     */
    public void gpuiShowKeyboard(int keyboardType, long session) {
        runOnUiThread(() -> {
            if (input == null) {
                input = new InputProxy();
                input.setAlpha(0f);
                input.setPadding(0, 0, 0, 0);
                addContentView(input, new ViewGroup.LayoutParams(1, 1));
            }
            input.reset(session);
            int type = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_MULTI_LINE;
            switch (keyboardType) {
                case 1: type = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS; break;
                case 2: type = InputType.TYPE_CLASS_PHONE; break;
                case 3: type = InputType.TYPE_CLASS_NUMBER; break;
                case 4: type = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI; break;
                case 5: type = InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_FLAG_DECIMAL; break;
            }
            input.setInputType(type);
            input.setImeOptions(EditorInfo.IME_FLAG_NO_EXTRACT_UI);
            input.requestFocus();
            InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
            imm.restartInput(input);
            imm.showSoftInput(input, InputMethodManager.SHOW_IMPLICIT);
        });
    }

    public void gpuiHideKeyboard(long session) {
        runOnUiThread(() -> {
            if (input == null) return;
            input.reset(session);
            InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
            imm.hideSoftInputFromWindow(input.getWindowToken(), 0);
            input.clearFocus();
        });
    }

    public void gpuiResetComposition(long session) {
        runOnUiThread(() -> {
            if (input == null) return;
            input.reset(session);
            ((InputMethodManager) getSystemService(INPUT_METHOD_SERVICE)).restartInput(input);
        });
    }

    /** The InputConnection host GPUI drives through {@code nativeIme}. */
    private final class InputProxy extends EditText {
        private long session;
        private int depth;
        private boolean marked;

        InputProxy() {
            super(GpuiHostActivity.this);
            addTextChangedListener(new TextWatcher() {
                public void beforeTextChanged(CharSequence s, int start, int count, int after) {}
                public void onTextChanged(CharSequence s, int start, int before, int count) {}
                public void afterTextChanged(Editable text) {
                    // Hardware keyboards edit the widget directly, outside its
                    // InputConnection. IME mutations are batched by depth below.
                    if (depth == 0) { depth++; endEdit(); }
                }
            });
        }

        @Override
        public boolean onKeyDown(int code, KeyEvent event) {
            if (code == KeyEvent.KEYCODE_DEL && getText().length() == 0 && !marked) {
                nativeIme(session, 3, "", 1, 0);
                return true;
            }
            return super.onKeyDown(code, event);
        }

        void reset(long nextSession) {
            depth++;
            getText().clear();
            marked = false;
            session = nextSession;
            depth = 0;
        }

        private void endEdit() {
            if (--depth != 0) return;
            Editable text = getText();
            boolean composing = BaseInputConnection.getComposingSpanStart(text) >= 0;
            if (composing || marked || text.length() > 0) {
                nativeIme(session, composing ? 0 : 1, text.toString(),
                        Math.max(0, Selection.getSelectionStart(text)),
                        Math.max(0, Selection.getSelectionEnd(text)));
                marked = composing;
                if (!composing) {
                    depth++;
                    text.clear();
                    depth--;
                }
            }
        }

        @Override
        public boolean onKeyPreIme(int code, KeyEvent event) {
            if (code == KeyEvent.KEYCODE_BACK && event.getAction() == KeyEvent.ACTION_UP) {
                nativeIme(session, 4, "", 0, 0);
            }
            return super.onKeyPreIme(code, event);
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo info) {
            InputConnection connection = super.onCreateInputConnection(info);
            if (connection == null) return null;
            final long connectionSession = session;
            return new InputConnectionWrapper(connection, false) {
                @Override public boolean beginBatchEdit() {
                    if (connectionSession != session) return false;
                    depth++;
                    return super.beginBatchEdit();
                }
                @Override public boolean endBatchEdit() {
                    if (connectionSession != session) return false;
                    boolean result = super.endBatchEdit();
                    if (depth > 0) endEdit();
                    return result;
                }
                @Override public boolean setComposingText(CharSequence text, int cursor) {
                    if (connectionSession != session) return false;
                    depth++;
                    try { return super.setComposingText(text, cursor); }
                    finally { endEdit(); }
                }
                @Override public boolean setComposingRegion(int start, int end) {
                    if (connectionSession != session) return false;
                    depth++;
                    try { return super.setComposingRegion(start, end); }
                    finally { endEdit(); }
                }
                @Override public boolean finishComposingText() {
                    if (connectionSession != session) return false;
                    depth++;
                    try { return super.finishComposingText(); }
                    finally { endEdit(); }
                }
                @Override public boolean commitText(CharSequence text, int cursor) {
                    if (connectionSession != session) return false;
                    depth++;
                    try { return super.commitText(text, cursor); }
                    finally { endEdit(); }
                }
                @Override public boolean setSelection(int start, int end) {
                    if (connectionSession != session) return false;
                    depth++;
                    try { return super.setSelection(start, end); }
                    finally { endEdit(); }
                }
                @Override public boolean deleteSurroundingText(int before, int after) {
                    if (connectionSession != session) return false;
                    if (getText().length() == 0 && !marked) {
                        nativeIme(session, 2, "", before, after);
                        return true;
                    }
                    depth++;
                    try { return super.deleteSurroundingText(before, after); }
                    finally { endEdit(); }
                }
                @Override public boolean deleteSurroundingTextInCodePoints(int before, int after) {
                    if (connectionSession != session) return false;
                    if (getText().length() == 0 && !marked) {
                        nativeIme(session, 3, "", before, after);
                        return true;
                    }
                    depth++;
                    try { return super.deleteSurroundingTextInCodePoints(before, after); }
                    finally { endEdit(); }
                }
                @Override public boolean sendKeyEvent(KeyEvent event) {
                    if (connectionSession != session) return false;
                    if (event.getKeyCode() == KeyEvent.KEYCODE_DEL) {
                        if (event.getAction() == KeyEvent.ACTION_DOWN) deleteSurroundingText(1, 0);
                        return true;
                    }
                    if (event.getKeyCode() == KeyEvent.KEYCODE_ENTER) {
                        if (event.getAction() == KeyEvent.ACTION_DOWN) commitText("\n", 1);
                        return true;
                    }
                    return super.sendKeyEvent(event);
                }
                @Override public boolean performEditorAction(int action) {
                    if (connectionSession != session) return false;
                    if (action == EditorInfo.IME_ACTION_DONE) {
                        finishComposingText();
                        nativeIme(session, 4, "", 0, 0);
                        return true;
                    }
                    return commitText("\n", 1);
                }
            };
        }
    }

    // ── notifications ────────────────────────────────────────────────────────

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

    public void showAgentNotification(
            String notificationId, String title, String body, String opaqueLocator) {
        runOnUiThread(() -> {
            Intent intent = new Intent(this, GpuiHostActivity.class)
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

    // ── background connection service ────────────────────────────────────────

    public void startRemoteConnectionService() {
        RemoteConnectionService.start(getApplicationContext());
    }

    public void stopRemoteConnectionService() {
        RemoteConnectionService.stop(getApplicationContext());
    }

    // ── battery allowlist ────────────────────────────────────────────────────

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

    // ── pairing ──────────────────────────────────────────────────────────────

    public void launchPairingQrScanner() {
        runOnUiThread(() ->
                startActivity(new Intent(this, PairingQrScannerActivity.class)));
    }

    // ── LAN discovery ────────────────────────────────────────────────────────

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
