//! Process hygiene shared by every agent-side child, plus the one Windows flag
//! the ACP spawn path adds on top of it.
//!
//! The AppImage policy itself lives in [`vibex_core::process_environment`]: the
//! computer-use helper launches the same kind of host binary from the same
//! mount, and a second copy of the rule is a second place for the two to drift.

use std::process::Command;

/// Remove the ACP child from the desktop's controlling terminal before the
/// process-group hook moves it into the background. Some CLIs open `/dev/tty`
/// for interactive services even in ACP mode; a background process group that
/// does so is stopped by `SIGTTIN` and never answers the ACP handshake.
///
/// `setsid()` would detach the child too, but it cannot be used here: the child
/// is spawned into its own process group, a session leader may not call
/// `setpgid`, and the group spawn registers that call immediately after this
/// hook. A `setsid()` in this hook would therefore fail every spawn.
#[cfg(unix)]
pub fn detach_from_controlling_terminal(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: the hook only performs async-signal-safe syscalls between fork
    // and exec. It does not allocate, lock, or access shared Rust state.
    unsafe {
        command.pre_exec(|| {
            const DEV_TTY: &[u8] = b"/dev/tty\0";
            // The child may not have a controlling terminal (for example in a
            // service or test process), so failure to open it is expected.
            let fd = libc::open(DEV_TTY.as_ptr().cast(), libc::O_RDWR | libc::O_CLOEXEC);
            if fd >= 0 {
                // `libc` names `TIOCNOTTY` for Linux and the BSDs but not for
                // Android, although the Android kernel implements the same
                // request, so Android names Linux's `<asm-generic/ioctls.h>`
                // value for itself instead of shipping without a detach.
                #[cfg(target_os = "android")]
                let request = 0x5422;
                #[cfg(not(target_os = "android"))]
                let request = libc::TIOCNOTTY;

                libc::ioctl(fd, request);
                libc::close(fd);
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub fn detach_from_controlling_terminal(_command: &mut Command) {}

#[cfg(windows)]
pub(crate) const WINDOWS_CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Configure ACP child processes without leaking AppImage paths or opening
/// standalone Windows console windows.
pub fn sanitize_inherited_appimage_environment(command: &mut Command) {
    vibex_core::process_environment::sanitize_inherited_appimage_environment(command);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn acp_child_does_not_inherit_the_controlling_terminal() {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(
            "if dd if=/dev/tty of=/dev/null count=0 2>/dev/null; then exit 42; else exit 0; fi",
        );
        detach_from_controlling_terminal(&mut command);

        let status = command.status().unwrap();

        assert!(status.success());
    }
}
