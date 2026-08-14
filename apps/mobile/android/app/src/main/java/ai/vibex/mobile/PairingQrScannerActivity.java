package ai.vibex.mobile;

import android.Manifest;
import android.content.pm.PackageManager;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.Size;
import android.view.Gravity;
import android.view.View;
import android.widget.FrameLayout;
import android.widget.ImageButton;
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

import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/** Full-screen camera scanner for the one-time Vibex desktop pairing QR code. */
public final class PairingQrScannerActivity extends AppCompatActivity {
    static {
        // This Activity may be restored directly after process death, without
        // GpuiNativeActivity first associating the Rust library with its ClassLoader.
        System.loadLibrary("vibex_mobile");
    }

    private static final int CAMERA_PERMISSION_REQUEST = 100;
    private static final String PAIRING_PREFIX = "vibex://open/";

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
        close.setContentDescription("Close scanner");
        close.setOnClickListener(view -> finish());
        FrameLayout.LayoutParams closeParams = new FrameLayout.LayoutParams(dp(48), dp(48));
        closeParams.gravity = Gravity.TOP | Gravity.END;
        closeParams.topMargin = dp(16);
        closeParams.rightMargin = dp(12);
        root.addView(close, closeParams);

        TextView hint = new TextView(this);
        hint.setText("Scan the pairing QR code in Vibex desktop");
        hint.setTextColor(0xEEFFFFFF);
        hint.setTextSize(15);
        hint.setGravity(Gravity.CENTER);
        FrameLayout.LayoutParams hintParams = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.WRAP_CONTENT);
        hintParams.gravity = Gravity.BOTTOM;
        hintParams.leftMargin = dp(24);
        hintParams.rightMargin = dp(24);
        hintParams.bottomMargin = dp(80);
        root.addView(hint, hintParams);

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
            Toast.makeText(this, "Camera access is required to scan the pairing code", Toast.LENGTH_LONG)
                    .show();
            finish();
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
                Toast.makeText(this, "The camera could not be opened", Toast.LENGTH_LONG).show();
                finish();
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
                        if (value != null && value.startsWith(PAIRING_PREFIX)) {
                            finishScan(value);
                            break;
                        }
                    }
                })
                .addOnCompleteListener(task -> imageProxy.close());
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
