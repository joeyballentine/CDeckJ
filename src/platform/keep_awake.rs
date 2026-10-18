//! Keeping the screen on and the machine awake during a set.

/// Keeps the screen on and the machine awake while alive.
pub struct KeepAwake {
    /// The ScreenSaver inhibit lasts as long as this connection.
    _session: Option<zbus::blocking::Connection>,
    /// logind releases the sleep/idle block when this descriptor closes.
    _logind: Option<zbus::zvariant::OwnedFd>,
}

/// Ask the desktop not to dim/lock (freedesktop ScreenSaver inhibit, held for as long as
/// the D-Bus connection lives) and logind not to idle-sleep (held by a file descriptor).
/// Both are best effort. (Talking to logind directly rather than running `systemd-inhibit`
/// also keeps the binary free of newer-glibc process-spawning symbols.)
pub fn keep_awake() -> KeepAwake {
    let session = zbus::blocking::Connection::session().ok().and_then(|c| {
        c.call_method(
            Some("org.freedesktop.ScreenSaver"),
            "/org/freedesktop/ScreenSaver",
            Some("org.freedesktop.ScreenSaver"),
            "Inhibit",
            &("CDeckJ", "DJ set in progress"),
        )
        .ok()
        .map(|_| c)
    });
    let logind = zbus::blocking::Connection::system().ok().and_then(|c| {
        let reply = c
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "Inhibit",
                &("idle:sleep", "CDeckJ", "DJ set in progress", "block"),
            )
            .ok()?;
        reply.body().deserialize::<zbus::zvariant::OwnedFd>().ok()
    });
    if session.is_none() && logind.is_none() {
        eprintln!("keep-awake: no screensaver or logind service; the screen may dim");
    }
    KeepAwake { _session: session, _logind: logind }
}
