package ai.vibex.mobile;

import android.app.NativeActivity;
import android.content.Context;
import android.content.Intent;
import android.graphics.Color;
import android.os.Bundle;
import android.text.Editable;
import android.text.InputType;
import android.text.TextWatcher;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputMethodManager;
import android.widget.EditText;
import android.widget.FrameLayout;

import androidx.core.view.WindowCompat;

/** NativeActivity host that supplies Android's IME with a real InputConnection. */
public final class GpuiNativeActivity extends NativeActivity {
    static {
        // NativeActivity's manifest loader does not register the library with
        // this ClassLoader, so Java-declared JNI callbacks need an explicit load.
        System.loadLibrary("vibex_mobile");
    }

    private GpuiEditText textInput;

    private static native void nativeReplaceText(int start, int before, String replacement);
    private static native void nativeSetSelection(int start, int end);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        // Keep the GPUI content rectangle below Android system bars. GPUI then
        // publishes the same geometry through Window::insets() as iOS does.
        WindowCompat.setDecorFitsSystemWindows(getWindow(), true);

        textInput = new GpuiEditText(this);
        FrameLayout.LayoutParams params = new FrameLayout.LayoutParams(1, 1);
        ViewGroup content = findViewById(android.R.id.content);
        content.addView(textInput, params);
    }

    /** Called from the GPUI thread; all View work is transferred to Android's UI thread. */
    public void showGpuiKeyboard(String text, int selectionStart, int selectionEnd) {
        runOnUiThread(() -> {
            textInput.syncDocument(text, selectionStart, selectionEnd);
            textInput.requestFocus();
            InputMethodManager manager =
                    (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            manager.restartInput(textInput);
            textInput.post(() -> manager.showSoftInput(textInput, InputMethodManager.SHOW_IMPLICIT));
        });
    }

    /** Keeps programmatic GPUI edits and cursor moves visible to the active IME. */
    public void syncGpuiText(String text, int selectionStart, int selectionEnd) {
        runOnUiThread(() -> textInput.syncDocument(text, selectionStart, selectionEnd));
    }

    public void hideGpuiKeyboard() {
        runOnUiThread(() -> {
            InputMethodManager manager =
                    (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            manager.hideSoftInputFromWindow(textInput.getWindowToken(), 0);
            textInput.clearFocus();
        });
    }

    public void launchPairingQrScanner() {
        runOnUiThread(() ->
                startActivity(new Intent(this, PairingQrScannerActivity.class)));
    }

    private static final class GpuiEditText extends EditText {
        private boolean synchronizing;

        GpuiEditText(Context context) {
            super(context);
            setSingleLine(true);
            setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
            setImeOptions(EditorInfo.IME_ACTION_NONE | EditorInfo.IME_FLAG_NO_EXTRACT_UI);
            setBackground(null);
            setTextColor(Color.TRANSPARENT);
            setHintTextColor(Color.TRANSPARENT);
            setCursorVisible(false);
            setPadding(0, 0, 0, 0);
            setImportantForAccessibility(View.IMPORTANT_FOR_ACCESSIBILITY_NO);
            addTextChangedListener(new TextWatcher() {
                @Override
                public void beforeTextChanged(CharSequence text, int start, int count, int after) {}

                @Override
                public void onTextChanged(CharSequence text, int start, int before, int count) {
                    if (!synchronizing && hasFocus()) {
                        nativeReplaceText(
                                start,
                                before,
                                text.subSequence(start, start + count).toString());
                    }
                }

                @Override
                public void afterTextChanged(Editable text) {}
            });
        }

        void syncDocument(String text, int selectionStart, int selectionEnd) {
            synchronizing = true;
            try {
                if (!getText().toString().equals(text)) {
                    setText(text);
                }
                int length = getText().length();
                int start = Math.max(0, Math.min(selectionStart, length));
                int end = Math.max(0, Math.min(selectionEnd, length));
                if (getSelectionStart() != start || getSelectionEnd() != end) {
                    setSelection(start, end);
                }
            } finally {
                synchronizing = false;
            }
        }

        @Override
        protected void onSelectionChanged(int start, int end) {
            super.onSelectionChanged(start, end);
            if (!synchronizing && hasFocus() && start >= 0 && end >= 0) {
                nativeSetSelection(start, end);
            }
        }
    }
}
