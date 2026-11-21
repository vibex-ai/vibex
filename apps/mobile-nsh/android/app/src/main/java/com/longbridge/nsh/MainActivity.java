package com.longbridge.nsh;

import android.app.Activity;
import android.app.DownloadManager;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.os.Environment;
import android.net.Uri;
import android.provider.MediaStore;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;

import ai.vibex.mobile.nsh.NshHost;

import androidx.core.content.FileProvider;

import java.io.File;

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
    private static final int CAMERA_REQUEST = 43;

    private NshView view;
    private LanDiscovery lanDiscovery;
    private final Handler handler = new Handler(Looper.getMainLooper());
    private boolean scanning;
    private String cameraPath;
    private long updateDownloadId = -1;
    private String updateApkPath;
    private BroadcastReceiver updateReceiver;

    private final Runnable poll = new Runnable() {
        @Override
        public void run() {
            int request = NshHost.nativeTakeHostRequest();
            if (request == 1 && !scanning) {
                scanning = true;
                openScanner();
            } else if (request == 2) {
                lanDiscovery.start();
            } else if (request == 3) {
                lanDiscovery.stop();
            } else if (request == 4) {
                openCamera();
            }
            String updateUrl = NshHost.nativeTakeUpdateUrl();
            if (updateUrl != null) {
                downloadUpdate(updateUrl);
            }
            int connectionService = NshHost.nativeTakeConnectionService();
            if (connectionService == 1) {
                ConnectionService.start(MainActivity.this);
            } else if (connectionService == 0) {
                ConnectionService.stop(MainActivity.this);
            }
            int awake = NshHost.nativeTakeKeepScreenOn();
            if (awake == 1) {
                getWindow().addFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            } else if (awake == 0) {
                getWindow().clearFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
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
        NshHost.nativeSetCameraAvailable(true);
        NshHost.nativeSetDiscoveryAvailable(true);
        lanDiscovery = new LanDiscovery(this, NshHost::nativeDiscoveryEvent);
        view = new NshView(this);
        view.useStandaloneLimits();
        setContentView(view);
        if (state == null) {
            view.handleUrlSchemeIntent(getIntent());
            readSharedText(getIntent());
            readShortcut(getIntent());
        }
        view.runRustApp("nsh");
        DevActivity.systemBars(this);
        updateReceiver = new BroadcastReceiver() {
            @Override
            public void onReceive(Context context, Intent intent) {
                long id = intent.getLongExtra(DownloadManager.EXTRA_DOWNLOAD_ID, -1);
                if (id == updateDownloadId && updateApkPath != null) {
                    installApk(updateApkPath);
                }
            }
        };
        IntentFilter downloads = new IntentFilter(DownloadManager.ACTION_DOWNLOAD_COMPLETE);
        if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.TIRAMISU) {
            // Android 13+ requires an explicit export flag at registration time;
            // the download manager's broadcast is a system broadcast, so the
            // receiver stays unexported.
            registerReceiver(updateReceiver, downloads, Context.RECEIVER_NOT_EXPORTED);
        } else {
            registerReceiver(updateReceiver, downloads);
        }
        handler.post(poll);
    }

    private void openScanner() {
        startActivityForResult(new Intent(this, ScannerActivity.class), SCAN_REQUEST);
    }

    private void downloadUpdate(String url) {
        try {
            File directory = new File(getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS), "updates");
            if (!directory.exists() && !directory.mkdirs()) {
                return;
            }
            File apk = new File(directory, "vibex-update.apk");
            updateApkPath = apk.getAbsolutePath();
            DownloadManager.Request request = new DownloadManager.Request(Uri.parse(url));
            request.setTitle("Vibex update");
            request.setMimeType("application/vnd.android.package-archive");
            request.setDestinationUri(Uri.fromFile(apk));
            request.setNotificationVisibility(DownloadManager.Request.VISIBILITY_VISIBLE_NOTIFY_COMPLETED);
            DownloadManager manager = (DownloadManager) getSystemService(Context.DOWNLOAD_SERVICE);
            updateDownloadId = manager.enqueue(request);
        } catch (Exception error) {
            updateDownloadId = -1;
            updateApkPath = null;
        }
    }

    private void installApk(String path) {
        try {
            File apk = new File(path);
            Uri uri = FileProvider.getUriForFile(this, getPackageName() + ".files", apk);
            Intent intent = new Intent(Intent.ACTION_VIEW);
            intent.setDataAndType(uri, "application/vnd.android.package-archive");
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_ACTIVITY_NEW_TASK);
            startActivity(intent);
        } catch (Exception error) {
            // The user can still install from the release page.
        }
    }

    private void openCamera() {
        try {
            File directory = new File(getCacheDir(), "camera");
            if (!directory.exists() && !directory.mkdirs()) {
                return;
            }
            File photo = new File(directory, "shot-" + System.currentTimeMillis() + ".jpg");
            cameraPath = photo.getAbsolutePath();
            Uri uri = FileProvider.getUriForFile(this, getPackageName() + ".files", photo);
            Intent intent = new Intent(MediaStore.ACTION_IMAGE_CAPTURE);
            intent.putExtra(MediaStore.EXTRA_OUTPUT, uri);
            intent.addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            startActivityForResult(intent, CAMERA_REQUEST);
        } catch (Exception error) {
            cameraPath = null;
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == CAMERA_REQUEST) {
            if (resultCode == RESULT_OK && cameraPath != null) {
                NshHost.nativeCameraPhoto(cameraPath);
            }
            cameraPath = null;
            return;
        }
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
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(requestCode, permissions, results);
        lanDiscovery.onRequestPermissionsResult(requestCode, results);
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
        readSharedText(intent);
        readShortcut(intent);
    }

    private void readShortcut(Intent intent) {
        if (intent != null
                && "ai.vibex.mobile.nsh.NEW_SESSION".equals(intent.getAction())) {
            NshHost.nativeNewSession();
        }
    }

    private void readSharedText(Intent intent) {
        if (intent == null || !Intent.ACTION_SEND.equals(intent.getAction())) {
            return;
        }
        String text = intent.getStringExtra(Intent.EXTRA_TEXT);
        if (text != null && !text.trim().isEmpty()) {
            NshHost.nativeShareText(text);
        }
    }

    @Override
    @SuppressWarnings("deprecation")
    public void onBackPressed() {
        // A closeable surface of the Rust app owns the press first: the
        // drawer, a file preview, the terminal, a sheet. Only when none is
        // open does NshView get it, and the system last.
        if (NshHost.nativeBack()) {
            return;
        }
        if (view == null || !view.handleBack()) {
            super.onBackPressed();
        }
    }

    @Override
    protected void onDestroy() {
        handler.removeCallbacks(poll);
        if (updateReceiver != null) {
            unregisterReceiver(updateReceiver);
            updateReceiver = null;
        }
        if (lanDiscovery != null) {
            lanDiscovery.stop();
        }
        if (view != null) {
            view.destroy();
        }
        super.onDestroy();
    }
}
