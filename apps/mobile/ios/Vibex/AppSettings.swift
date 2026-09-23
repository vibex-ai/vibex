import UIKit

/// Opens this app's page in the system settings.
///
/// A denied local-network permission can only be re-granted there, and the
/// pairing screen that reports the denial has no other way to send the user to
/// it. Kept next to the other host shims rather than in the scanner because it
/// is a general host capability.
@_cdecl("vibex_ios_open_app_settings")
public func vibexIosOpenAppSettings() {
    DispatchQueue.main.async {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
    }
}
