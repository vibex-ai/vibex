package com.longbridge.nsh;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;

import ai.vibex.mobile.nsh.NshHost;

/**
 * The Vibex host: mounts the Rust app into an {@link NshView} and lends it the
 * phone's camera for pairing QR codes.
 *
 * <p>The app asks for the scanner through a small polled queue
 * ({@link NshHost#nativeTakeHostRequest()}), and the result goes back with
 * {@link NshHost#nativeScanResult(String)}. Everything else — the session
 * shell, discovery and the photo fallback — lives in the Rust app.
 */
public final class MainActivity extends Activity {
    private static final int SCAN_REQUEST = 42;

    private NshView view;
    private final Handler handler = new Handler(Looper.getMainLooper());
    private boolean scanning;

    private final Runnable poll = new Runnable() {
        @Override
        public void run() {
            if (NshHost.nativeTakeHostRequest() == 1 && !scanning) {
                scanning = true;
                openScanner();
            }
            handler.postDelayed(this, 200);
        }
    };

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        DevActivity.edgeToEdge(this);
        NshHost.nativeSetDeviceName(android.os.Build.MODEL);
        NshHost.nativeSetScannerAvailable(true);
        view = new NshView(this);
        view.useStandaloneLimits();
        setContentView(view);
        if (state == null) {
            view.handleUrlSchemeIntent(getIntent());
        }
        view.runRustApp("nsh");
        DevActivity.systemBars(this);
        handler.post(poll);
    }

    private void openScanner() {
        startActivityForResult(new Intent(this, ScannerActivity.class), SCAN_REQUEST);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != SCAN_REQUEST) {
            return;
        }
        scanning = false;
        if (resultCode == RESULT_OK && data != null) {
            String value = data.getStringExtra(ScannerActivity.EXTRA_RESULT);
            if (value != null) {
                NshHost.nativeScanResult(value);
            }
        }
    }

    @Override
    public void onConfigurationChanged(android.content.res.Configuration config) {
        super.onConfigurationChanged(config);
        DevActivity.systemBars(this);
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        if (view != null) {
            view.handleUrlSchemeIntent(intent);
        }
    }

    @Override
    @SuppressWarnings("deprecation")
    public void onBackPressed() {
        if (view == null || !view.handleBack()) {
            super.onBackPressed();
        }
    }

    @Override
    protected void onDestroy() {
        handler.removeCallbacks(poll);
        if (view != null) {
            view.destroy();
        }
        super.onDestroy();
    }
}
