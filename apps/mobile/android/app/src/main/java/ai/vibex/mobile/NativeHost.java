package ai.vibex.mobile;

/**
 * The Rust half of the host: JNI symbols exported by libnsh.so (this app's
 * Rust crate). The library is loaded by {@link com.longbridge.nsh.NshView}.
 */
public final class NativeHost {
    static {
        // The same library NshView loads; loading it here lets the host
        // report the device name before the view is made.
        System.loadLibrary("nsh");
    }

    private NativeHost() {}

    /** A pairing link the camera read. */
    public static native void nativeScanResult(String value);

    /** Tells the app that the host can open a camera. */
    public static native void nativeSetCameraAvailable(boolean available);

    /** The path of a photo the camera host wrote. */
    public static native void nativeCameraPhoto(String path);

    /** Why the camera host could not take a photo. */
    public static native void nativeCameraFailed(String reason);

    /** A launcher shortcut asked for a new session. */
    public static native void nativeNewSession();

    /** The APK URL the host should download and install, or null. */
    public static native String nativeTakeUpdateUrl();

    /** Text another app shared into Vibex. */
    public static native void nativeShareText(String value);

    /** One local-network discovery event, as the JSON contract describes. */
    public static native void nativeDiscoveryEvent(String value);

    /** The pending connection-service change: -1 none, 0 stop, 1 start. */
    public static native int nativeTakeConnectionService();

    /** The pending screen-awake change: -1 none, 0 off, 1 on. */
    public static native int nativeTakeKeepScreenOn();

    /**
     * The next thing the app asks the host to do: 0 none, 1 scan, 2 find
     * start, 3 find stop, 4 camera, 5 app settings.
     */
    public static native int nativeTakeHostRequest();

    /** Names the phone in the runtime's paired-device list. */
    public static native void nativeSetDeviceName(String value);

    /** Tells the app that Scan can use the host's camera. */
    public static native void nativeSetScannerAvailable(boolean available);

    /** Tells the app that Find can use the host's NSD discovery. */
    public static native void nativeSetDiscoveryAvailable(boolean available);
}
