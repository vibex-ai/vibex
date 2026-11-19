package com.longbridge.nsh;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.drawable.GradientDrawable;
import android.net.Uri;
import android.os.Bundle;
import android.view.Gravity;
import android.view.View;
import android.widget.FrameLayout;
import android.widget.ImageButton;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

import com.google.zxing.BarcodeFormat;
import com.google.zxing.BinaryBitmap;
import com.google.zxing.MultiFormatReader;
import com.google.zxing.Result;
import com.google.zxing.RGBLuminanceSource;
import com.google.zxing.common.HybridBinarizer;
import com.journeyapps.barcodescanner.BarcodeCallback;
import com.journeyapps.barcodescanner.BarcodeResult;
import com.journeyapps.barcodescanner.BarcodeView;
import com.journeyapps.barcodescanner.DefaultDecoderFactory;

import java.io.InputStream;
import java.util.Collections;
import java.util.List;
import java.util.Locale;

/**
 * The pairing scanner, matching the GPUI client's: full-screen camera,
 * viewfinder, close, a hint, and a gallery fallback for when the code is
 * already a screenshot or the camera is unavailable.
 *
 * <p>Only a {@code vibex://} pairing entry is returned; any other QR keeps the
 * scanner open so the person can try again, and a picked image without one
 * says so without closing.
 */
public final class ScannerActivity extends Activity {
    static final String EXTRA_RESULT = "com.longbridge.nsh.SCANNER_RESULT";

    private static final int CAMERA_REQUEST = 101;
    private static final int PICK_IMAGE_REQUEST = 102;

    private BarcodeView camera;
    private boolean finished;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);

        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(Color.BLACK);

        camera = new BarcodeView(this);
        camera.setDecoderFactory(
                new DefaultDecoderFactory(Collections.singletonList(BarcodeFormat.QR_CODE)));
        camera.decodeContinuous(new BarcodeCallback() {
            @Override
            public void barcodeResult(BarcodeResult result) {
                onCode(result.getText());
            }

            @Override
            public void possibleResultPoints(List<com.google.zxing.ResultPoint> points) {}
        });
        root.addView(camera, matchParent());
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

        TextView pick = new TextView(this);
        pick.setText(tr("Choose from photos", "从相册选择", "從相簿選擇"));
        pick.setTextColor(Color.WHITE);
        pick.setTextSize(15);
        pick.setGravity(Gravity.CENTER);
        pick.setPadding(dp(24), dp(12), dp(24), dp(12));
        GradientDrawable pill = new GradientDrawable();
        pill.setColor(0x33FFFFFF);
        pill.setCornerRadius(dp(24));
        pill.setStroke(dp(1), 0x66FFFFFF);
        pick.setBackground(pill);
        pick.setClickable(true);
        pick.setFocusable(true);
        pick.setOnClickListener(view -> pickPhoto());
        LinearLayout.LayoutParams pickParams = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT,
                LinearLayout.LayoutParams.WRAP_CONTENT);
        pickParams.topMargin = dp(16);
        footer.addView(pick, pickParams);

        FrameLayout.LayoutParams footerParams = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.WRAP_CONTENT);
        footerParams.gravity = Gravity.BOTTOM;
        footerParams.leftMargin = dp(24);
        footerParams.rightMargin = dp(24);
        footerParams.bottomMargin = dp(56);
        root.addView(footer, footerParams);

        setContentView(root);

        if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            camera.resume();
        } else {
            requestPermissions(new String[] {Manifest.permission.CAMERA}, CAMERA_REQUEST);
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(requestCode, permissions, results);
        if (requestCode != CAMERA_REQUEST) {
            return;
        }
        if (results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
            camera.resume();
        } else {
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

    private void onCode(String value) {
        if (finished || value == null) {
            return;
        }
        if (!isPairingEntry(value)) {
            Toast.makeText(
                            this,
                            tr(
                                    "That is not a Vibex pairing code",
                                    "这不是 Vibex 配对码",
                                    "這不是 Vibex 配對碼"),
                            Toast.LENGTH_SHORT)
                    .show();
            return;
        }
        finished = true;
        camera.pause();
        Intent data = new Intent();
        data.putExtra(EXTRA_RESULT, value);
        setResult(RESULT_OK, data);
        camera.postDelayed(this::finish, 300);
    }

    private static boolean isPairingEntry(String value) {
        return value.startsWith("vibex://open/") || value.startsWith("vibex://pair#");
    }

    private void pickPhoto() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("image/*");
        try {
            startActivityForResult(intent, PICK_IMAGE_REQUEST);
        } catch (android.content.ActivityNotFoundException error) {
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
            decodePicked(uri);
        }
    }

    /** Decodes a pairing entry out of an image the person picked. */
    private void decodePicked(Uri uri) {
        String value = null;
        try (InputStream input = getContentResolver().openInputStream(uri)) {
            Bitmap bitmap = BitmapFactory.decodeStream(input);
            if (bitmap != null) {
                int width = bitmap.getWidth();
                int height = bitmap.getHeight();
                int[] pixels = new int[width * height];
                bitmap.getPixels(pixels, 0, width, 0, 0, width, height);
                RGBLuminanceSource source = new RGBLuminanceSource(width, height, pixels);
                BinaryBitmap image = new BinaryBitmap(new HybridBinarizer(source));
                Result result = new MultiFormatReader().decode(image);
                value = result.getText();
            }
        } catch (Exception ignored) {
            // A missing or unreadable image is the same as no code.
        }
        if (value != null && isPairingEntry(value)) {
            onCode(value);
        } else {
            Toast.makeText(
                            this,
                            tr(
                                    "No Vibex pairing code in that image",
                                    "这张图片里没有 Vibex 配对码",
                                    "這張圖片裡沒有 Vibex 配對碼"),
                            Toast.LENGTH_LONG)
                    .show();
        }
    }

    @Override
    public void onBackPressed() {
        finish();
    }

    @Override
    protected void onPause() {
        camera.pause();
        super.onPause();
    }

    @Override
    protected void onResume() {
        super.onResume();
        if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            camera.resume();
        }
    }

    @Override
    protected void onDestroy() {
        camera.pause();
        super.onDestroy();
    }

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
            traditional = "Hant".equalsIgnoreCase(locale.getScript());
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

    /** The scrim and corner brackets around the scan area. */
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
