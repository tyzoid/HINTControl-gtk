# T-Mobile Home Internet Control (gtk)

Linux-only Rust + GTK4 rewrite of the desktop T-Mobile Home Internet Control app.

This project is based on the original
[HINT Control](https://github.com/zacharee/HINTControl) project. Thanks to
[Zachary Wander](https://github.com/zacharee) for creating it.

## Scope

This version intentionally drops the Kotlin Multiplatform Android/iOS targets. Rust owns gateway settings, HTTP authentication, gateway status refreshes, Wi-Fi radio updates, and reboot requests. GTK4 provides the Linux desktop UI with standard server-side decorations requested.

Supported in this initial rewrite:

- Common T-Mobile gateway API on `http://<gateway>:8080/TMI/v1`.
- Login with bearer-token auth.
- Live 5G/LTE signal graphs with a fixed 120-second time axis.
- Device details, clients, and Wi-Fi views.
- Wi-Fi radio and SSID saves.
- Gateway reboot.
- Settings persistence at `~/.config/hint-control/settings.json`.

Nokia-specific endpoints, translations, charted snapshots, update checks, and mobile widgets are not included.

## Development Build

Install Rust and GTK4 development files, then run:

```sh
cargo run
```

On Debian/Ubuntu-like systems the GTK package is typically `libgtk-4-dev`; on Arch it is `gtk4`.
