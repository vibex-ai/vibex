package ai.vibex.mobile;

import android.Manifest;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.drawable.GradientDrawable;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.Size;
import android.view.Gravity;
import android.view.View;
import android.widget.FrameLayout;
import android.widget.ImageButton;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

import androidx.annotation.NonNull;
import androidx.appcompat.app.AppCompatActivity;
import androidx.camera.core.CameraSelector;
import androidx.camera.core.ImageAnalysis;
import androidx.camera.core.ImageProxy;
import androidx.camera.core.Preview;
import androidx.camera.lifecycle.ProcessCameraProvider;
import androidx.camera.view.PreviewView;
import androidx.core.app.ActivityCompat;
import androidx.core.content.ContextCompat;

import com.google.common.util.concurrent.ListenableFuture;
import com.google.mlkit.vision.barcode.BarcodeScanner;
import com.google.mlkit.vision.barcode.BarcodeScannerOptions;
import com.google.mlkit.vision.barcode.BarcodeScanning;
import com.google.mlkit.vision.barcode.common.Barcode;
import com.google.mlkit.vision.common.InputImage;

import java.io.IOException;
import java.util.Locale;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Full-screen scanner for the one-time Vibex pairing entry.
 *
 * Both pairing targets reach the phone as a {@code vibex://} link — a desktop
 * advertises {@code #/pair/}, a headless runtime prints {@code #/code/} — so
 * one scanner covers both and the user never has to say which one they hold.
 *
 * The camera is the primary input, but not the only one: the code is often
 * already on the device as a screenshot, or the camera is unavailable, so the
 * same ML Kit reader also decodes an image the user picks from their gallery.
 * The gallery path needs no runtime permission because it goes through the
 * system document picker, which grants read access to the single picked item.
 */
public final class PairingQrScannerActivity extends AppCompatActivity {
    static {
        // This Activity may be restored directly after process death, without
        // GpuiNativeActivity first associating the Rust library with its ClassLoader.
        System.loadLibrary("vibex_mobile");
    }

    private static final int CAMERA_PERMISSION_REQUEST = 100;
    private static final int PICK_IMAGE_REQUEST = 101;
    private static final String PAIRING_PREFIX = "vibex://open/";
    // A vibex-server console prints a connection string and a QR rendering of
    // it; the same scanner accepts both pairing entry points.
    private static final String SERVER_PAIRING_PREFIX = "vibex://pair#";

    private static boolean isPairingEntry(String value) {
        return value.startsWith(PAIRING_PREFIX) || value.startsWith(SERVER_PAIRING_PREFIX);
    }

    private static native void nativeOnPairingQrScanned(String value);

    private PreviewView previewView;
    private BarcodeScanner barcodeScanner;
    private ExecutorService cameraExecutor;
    private ProcessCameraProvider cameraProvider;
    private boolean scanComplete;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(Color.BLACK);

        previewView = new PreviewView(this);
        root.addView(previewView, matchParent());
        root.addView(new ViewfinderOverlay(this), matchParent());

        ImageButton close = new ImageButton(this);
        close.setImageResource(android.R.drawable.ic_menu_close_clear_cancel);
        close.setColorFilter(Color.WHITE);
        close.setBackgroundColor(Color.TRANSPARENT);
        close.setContentDescription(tr("Close scanner", "关闭扫码", "關閉掃描"));
        close.setOnClickListener(view -> finish());
        FrameLayout.LayoutParams closeParams = new FrameLayout.LayoutParams(dp(48), dp(48));
        closeParams.gravity = Gravity.TOP | Gravity.END;
        closeParams.topMargin = dp(16);
        closeParams.rightMargin = dp(12);
        root.addView(close, closeParams);

        LinearLayout footer = new LinearLayout(this);
        footer.setOrientation(LinearLayout.VERTICAL);
        footer.setGravity(Gravity.CENTER_HORIZONTAL);

        TextView hint = new TextView(this);
        hint.setText(tr(
                "Scan the pairing QR code shown by Vibex, or pick a screenshot",
                "扫描 Vibex 显示的配对二维码，或选择一张截图",
                "掃描 Vibex 顯示的配對 QR Code，或選擇一張截圖"));
        hint.setTextColor(0xEEFFFFFF);
        hint.setTextSize(15);
        hint.setGravity(Gravity.CENTER);
        footer.addView(hint);

        TextView pickImage = new TextView(this);
        pickImage.setText(tr("Choose from photos", "从相册选择", "從相簿選擇"));
        pickImage.setTextColor(Color.WHITE);
        pickImage.setTextSize(15);
        pickImage.setGravity(Gravity.CENTER);
        pickImage.setPadding(dp(24), dp(12), dp(24), dp(12));
        GradientDrawable pill = new GradientDrawable();
        pill.setColor(0x33FFFFFF);
        pill.setCornerRadius(dp(24));
        pill.setStroke(dp(1), 0x66FFFFFF);
        pickImage.setBackground(pill);
        pickImage.setClickable(true);
        pickImage.setFocusable(true);
        pickImage.setOnClickListener(view -> pickImage());
        LinearLayout.LayoutParams pickParams = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT,
                LinearLayout.LayoutParams.WRAP_CONTENT);
        pickParams.topMargin = dp(16);
        footer.addView(pickImage, pickParams);

        FrameLayout.LayoutParams footerParams = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.WRAP_CONTENT);
        footerParams.gravity = Gravity.BOTTOM;
        footerParams.leftMargin = dp(24);
        footerParams.rightMargin = dp(24);
        footerParams.bottomMargin = dp(56);
        root.addView(footer, footerParams);

        setContentView(root);

        BarcodeScannerOptions options = new BarcodeScannerOptions.Builder()
                .setBarcodeFormats(Barcode.FORMAT_QR_CODE)
                .build();
        barcodeScanner = BarcodeScanning.getClient(options);
        cameraExecutor = Executors.newSingleThreadExecutor();

        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA)
                == PackageManager.PERMISSION_GRANTED) {
            startCamera();
        } else {
            ActivityCompat.requestPermissions(
                    this,
                    new String[]{Manifest.permission.CAMERA},
                    CAMERA_PERMISSION_REQUEST);
        }
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode,
            @NonNull String[] permissions,
            @NonNull int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode != CAMERA_PERMISSION_REQUEST) {
            return;
        }
        if (grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
            startCamera();
        } else {
            // The gallery path still works without the camera, so the scanner
            // stays open instead of closing on a denial.
            Toast.makeText(
                            this,
                            tr(
                                    "Camera access is off. Pick a screenshot instead",
                                    "没有相机权限，请改用从相册选择",
                                    "沒有相機權限，請改從相簿選擇"),
                            Toast.LENGTH_LONG)
                    .show();
        }
    }

    private void startCamera() {
        ListenableFuture<ProcessCameraProvider> providerFuture =
                ProcessCameraProvider.getInstance(this);
        providerFuture.addListener(() -> {
            try {
                cameraProvider = providerFuture.get();
                Preview preview = new Preview.Builder()
                        .setTargetResolution(new Size(1280, 720))
                        .build();
                preview.setSurfaceProvider(previewView.getSurfaceProvider());

                ImageAnalysis analysis = new ImageAnalysis.Builder()
                        .setTargetResolution(new Size(1280, 720))
                        .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                        .build();
                analysis.setAnalyzer(cameraExecutor, this::analyzeImage);

                cameraProvider.unbindAll();
                cameraProvider.bindToLifecycle(
                        this,
                        CameraSelector.DEFAULT_BACK_CAMERA,
                        preview,
                        analysis);
            } catch (Exception error) {
                Toast.makeText(
                                this,
                                tr(
                                        "The camera could not be opened. Pick a screenshot instead",
                                        "无法打开相机，请改用从相册选择",
                                        "無法開啟相機，請改從相簿選擇"),
                                Toast.LENGTH_LONG)
                        .show();
            }
        }, ContextCompat.getMainExecutor(this));
    }

    @SuppressWarnings("UnsafeOptInUsageError")
    private void analyzeImage(ImageProxy imageProxy) {
        if (scanComplete || imageProxy.getImage() == null) {
            imageProxy.close();
            return;
        }
        InputImage image = InputImage.fromMediaImage(
                imageProxy.getImage(),
                imageProxy.getImageInfo().getRotationDegrees());
        barcodeScanner.process(image)
                .addOnSuccessListener(barcodes -> {
                    for (Barcode barcode : barcodes) {
                        String value = barcode.getRawValue();
                        if (value != null && isPairingEntry(value)) {
                            finishScan(value);
                            break;
                        }
                    }
                })
                .addOnCompleteListener(task -> imageProxy.close());
    }

    /**
     * Opens the system document picker for a single image.
     *
     * The modern photo picker is not used because it is absent on some devices
     * this app still supports, and it would add a dependency for a picker the
     * document UI already provides. Neither path needs a runtime permission.
     */
    private void pickImage() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("image/*");
        try {
            startActivityForResult(intent, PICK_IMAGE_REQUEST);
            return;
        } catch (ActivityNotFoundException ignored) {
            // Fall through to the older gallery picker.
        }
        Intent fallback = new Intent(Intent.ACTION_GET_CONTENT);
        fallback.addCategory(Intent.CATEGORY_OPENABLE);
        fallback.setType("image/*");
        try {
            startActivityForResult(fallback, PICK_IMAGE_REQUEST);
        } catch (ActivityNotFoundException ignored) {
            Toast.makeText(
                            this,
                            tr(
                                    "No photo picker is available on this device",
                                    "此设备上没有可用的图片选择器",
                                    "此裝置上沒有可用的圖片選擇器"),
                            Toast.LENGTH_LONG)
                    .show();
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != PICK_IMAGE_REQUEST || resultCode != RESULT_OK || data == null) {
            return;
        }
        Uri uri = data.getData();
        if (uri != null) {
            scanPickedImage(uri);
        }
    }

    /** Decodes a pairing entry out of an image the user picked. */
    private void scanPickedImage(Uri uri) {
        InputImage image;
        try {
            image = InputImage.fromFilePath(this, uri);
        } catch (IOException error) {
            showNoPairingCodeFound();
            return;
        }
        barcodeScanner.process(image)
                .addOnSuccessListener(barcodes -> {
                    for (Barcode barcode : barcodes) {
                        String value = barcode.getRawValue();
                        if (value != null && isPairingEntry(value)) {
                            finishScan(value);
                            return;
                        }
                    }
                    showNoPairingCodeFound();
                })
                .addOnFailureListener(error -> showNoPairingCodeFound());
    }

    /**
     * Reports an image that carries no usable entry.
     *
     * Staying on the scanner is the point: the user almost always picked the
     * wrong screenshot, and closing would make them reopen the scanner to try
     * the right one.
     */
    private void showNoPairingCodeFound() {
        Toast.makeText(
                        this,
                        tr(
                                "No Vibex pairing code in that image",
                                "这张图片里没有 Vibex 配对码",
                                "這張圖片裡沒有 Vibex 配對碼"),
                        Toast.LENGTH_LONG)
                .show();
    }

    private void finishScan(String value) {
        if (scanComplete) {
            return;
        }
        scanComplete = true;
        nativeOnPairingQrScanned(value);
        runOnUiThread(() -> {
            releaseCamera();
            new Handler(Looper.getMainLooper()).postDelayed(this::finish, 700);
        });
    }

    private void releaseCamera() {
        if (cameraProvider != null) {
            cameraProvider.unbindAll();
        }
        if (previewView != null && previewView.getParent() instanceof FrameLayout) {
            ((FrameLayout) previewView.getParent()).removeView(previewView);
        }
    }

    @Override
    protected void onDestroy() {
        releaseCamera();
        if (barcodeScanner != null) {
            barcodeScanner.close();
        }
        if (cameraExecutor != null) {
            cameraExecutor.shutdown();
        }
        super.onDestroy();
    }

    /** Picks the copy for the device language, matching the in-app locales. */
    private String tr(String en, String zhCn, String zhTw) {
        Locale locale = getResources().getConfiguration().getLocales().get(0);
        if (!"zh".equals(locale.getLanguage())) {
            return en;
        }
        String region = locale.getCountry();
        boolean traditional = "TW".equalsIgnoreCase(region)
                || "HK".equalsIgnoreCase(region)
                || "MO".equalsIgnoreCase(region);
        if (!traditional) {
            String script = locale.getScript();
            traditional = "Hant".equalsIgnoreCase(script);
        }
        return traditional ? zhTw : zhCn;
    }

    private FrameLayout.LayoutParams matchParent() {
        return new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.MATCH_PARENT);
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private static final class ViewfinderOverlay extends View {
        private final Paint scrim = new Paint();
        private final Paint corners = new Paint();

        ViewfinderOverlay(android.content.Context context) {
            super(context);
            scrim.setColor(0x99000000);
            corners.setColor(Color.WHITE);
            corners.setStyle(Paint.Style.STROKE);
            corners.setStrokeWidth(5 * context.getResources().getDisplayMetrics().density);
            corners.setStrokeCap(Paint.Cap.SQUARE);
        }

        @Override
        protected void onDraw(Canvas canvas) {
            super.onDraw(canvas);
            float density = getResources().getDisplayMetrics().density;
            float side = Math.min(280 * density, getWidth() - 48 * density);
            float left = (getWidth() - side) / 2;
            float top = (getHeight() - side) * 0.42f;
            float right = left + side;
            float bottom = top + side;

            canvas.drawRect(0, 0, getWidth(), top, scrim);
            canvas.drawRect(0, bottom, getWidth(), getHeight(), scrim);
            canvas.drawRect(0, top, left, bottom, scrim);
            canvas.drawRect(right, top, getWidth(), bottom, scrim);

            float arm = 28 * density;
            canvas.drawLine(left, top, left + arm, top, corners);
            canvas.drawLine(left, top, left, top + arm, corners);
            canvas.drawLine(right, top, right - arm, top, corners);
            canvas.drawLine(right, top, right, top + arm, corners);
            canvas.drawLine(left, bottom, left + arm, bottom, corners);
            canvas.drawLine(left, bottom, left, bottom - arm, corners);
            canvas.drawLine(right, bottom, right - arm, bottom, corners);
            canvas.drawLine(right, bottom, right, bottom - arm, corners);
        }
    }
}
