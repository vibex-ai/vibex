package ai.vibex.mobile.nsh;

/**
 * The Rust half of the host: JNI symbols exported by libnsh.so (this app's
 * Rust crate). The library is loaded by {@link com.longbridge.nsh.NshView}.
 */
public final class NshHost {
    static {
        // The same library NshView loads; loading it here lets the host
        // report the device name before the view is made.
        System.loadLibrary("nsh");
    }

    private NshHost() {}

    /** A pairing link the camera read. */
    public static native void nativeScanResult(String value);

    /** The next thing the app asks the host to do: 0 none, 1 scan. */
    public static native int nativeTakeHostRequest();

    /** Names the phone in the runtime's paired-device list. */
    public static native void nativeSetDeviceName(String value);

    /** Tells the app that Scan can use the host's camera. */
    public static native void nativeSetScannerAvailable(boolean available);
}
