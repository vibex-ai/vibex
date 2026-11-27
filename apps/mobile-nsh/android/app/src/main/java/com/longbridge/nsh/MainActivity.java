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
    private static final int CAMERA_PERMISSION_REQUEST = 44;

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
            } else if (request == 5) {
                openAppSettings();
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
        // Made again for a configuration change it does not take as it runs —
        // an overlay changing its resources, such as the navigation mode or
        // the wallpaper's colors — the activity takes over the app that was
        // already running instead of starting a second one. Without this the
        // new activity built a fresh NshView and the session, the draft and
        // every open surface went with the old one.
        Object last = getLastNonConfigurationInstance();
        if (last instanceof Kept) {
            Kept kept = (Kept) last;
            view = kept.view;
            // Its views and the windows it shows over the activity come back
            // as they were, and it hears the new activity's environment.
            view.enterActivity(this);
            DevActivity.systemBars(this);
        } else {
            // A `MutableContextWrapper`, so the view can move to the activity
            // the system makes next: `NshView.canMove` is false for a view
            // built on the activity itself, and nothing would be kept.
            view = new NshView(new android.content.MutableContextWrapper(this));
            view.useStandaloneLimits();
            setContentView(view);
            if (state == null) {
                view.handleUrlSchemeIntent(getIntent());
                readSharedText(getIntent());
                readShortcut(getIntent());
            }
            view.runRustApp("nsh");
            DevActivity.systemBars(this);
        }
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
        // The app declares CAMERA, and Android refuses the capture intent to
        // an app that declares it without holding it. Ask first; the capture
        // is started from the answer.
        if (checkSelfPermission(android.Manifest.permission.CAMERA)
                != android.content.pm.PackageManager.PERMISSION_GRANTED) {
            requestPermissions(
                    new String[] {android.Manifest.permission.CAMERA},
                    CAMERA_PERMISSION_REQUEST);
            return;
        }
        startCapture();
    }

    private void startCapture() {
        try {
            File directory = new File(getCacheDir(), "camera");
            if (!directory.exists() && !directory.mkdirs()) {
                NshHost.nativeCameraFailed("the camera directory could not be made");
                return;
            }
            File photo = new File(directory, "shot-" + System.currentTimeMillis() + ".jpg");
            cameraPath = photo.getAbsolutePath();
            Uri uri = FileProvider.getUriForFile(this, getPackageName() + ".files", photo);
            Intent intent = new Intent(MediaStore.ACTION_IMAGE_CAPTURE);
            intent.putExtra(MediaStore.EXTRA_OUTPUT, uri);
            intent.addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            // A phone with no camera app is a phone with nothing to resolve
            // this intent; without the check startActivityForResult throws and
            // the app looked as if the button did nothing.
            if (intent.resolveActivity(getPackageManager()) == null) {
                cameraPath = null;
                NshHost.nativeCameraFailed("no camera app is installed");
                return;
            }
            startActivityForResult(intent, CAMERA_REQUEST);
        } catch (Exception error) {
            cameraPath = null;
            NshHost.nativeCameraFailed(
                    error.getMessage() == null ? error.toString() : error.getMessage());
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == CAMERA_REQUEST) {
            if (resultCode == RESULT_OK && cameraPath != null) {
                NshHost.nativeCameraPhoto(cameraPath);
            } else if (resultCode != RESULT_CANCELED) {
                // A cancelled capture is the reader changing their mind and
                // says nothing; a failed one would otherwise be silent.
                NshHost.nativeCameraFailed("the camera returned no photo");
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
        if (requestCode == CAMERA_PERMISSION_REQUEST) {
            if (results.length > 0
                    && results[0] == android.content.pm.PackageManager.PERMISSION_GRANTED) {
                startCapture();
            } else {
                NshHost.nativeCameraFailed("camera access was refused");
            }
            return;
        }
        lanDiscovery.onRequestPermissionsResult(requestCode, results);
    }

    @Override
    public void onConfigurationChanged(android.content.res.Configuration config) {
        super.onConfigurationChanged(config);
        DevActivity.systemBars(this);
        // A change the running activity does take (the window resized, the
        // appearance switched): the bars are this activity's to redraw, and
        // the view has to apply them again.
        if (view != null) {
            view.mounter().applySystemBars();
        }
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

    private void openAppSettings() {
        Intent intent = new Intent(android.provider.Settings.ACTION_APPLICATION_DETAILS_SETTINGS);
        intent.setData(android.net.Uri.fromParts("package", getPackageName(), null));
        startActivity(intent);
    }

    private void readShortcut(Intent intent) {
        // The action ships as the GPUI client's: a shortcut the launcher saved
        // before the upgrade still fires after it, and the string is the app's
        // identity, not this package's.
        if (intent != null
                && "ai.vibex.mobile.NEW_SESSION".equals(intent.getAction())) {
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
        // The activity goes, the app does not: the system is making this one
        // again for a configuration change, and `onCreate` will take the view
        // over. Only a real exit destroys it.
        if (isChangingConfigurations() && view != null && view.canMove()) {
            view.leaveActivity();
        } else if (view != null) {
            view.destroy();
        }
        super.onDestroy();
    }

    /**
     * What the activity made again for a configuration change takes over: the
     * running app, not a new one.
     *
     * The view is kept only when it can move ({@link NshView#canMove}), which
     * needs the {@link android.content.MutableContextWrapper} `onCreate` gives
     * it; otherwise the system gets `null` and starts the app over, as before.
     */
    @Override
    public Object onRetainNonConfigurationInstance() {
        return view != null && view.canMove() ? new Kept(view) : null;
    }

    /** The running app, handed to the activity the system makes next. */
    private static final class Kept {
        final NshView view;

        Kept(NshView view) {
            this.view = view;
        }
    }
}
