# Colony Command on a phone or tablet

The mobile app is the same map in a Tauri 2 webview. It has no daemon, tray or
installer of its own: it connects to the Colony Command on your PC over your VPN,
after you pair it (see [remote-access.md](remote-access.md) for the PC side and the
threat model). What it can do on a given device is what the PC granted that device:
view only, or control.

## Android

### Get a build

CI builds a debug APK on every pull request that touches `app/` (the **Android**
workflow). Open the run, download the **colony-command-android-debug** artifact and
unzip it.

### Install it on a device (sideload)

1. On the device, allow installs from your browser or file manager
   (Settings, Apps, Special access, Install unknown apps).
2. Copy the `.apk` to the device and open it. Or, with USB debugging on and the
   device plugged in: `adb install -r app-universal-debug.apk` (the file name depends
   on the build; use the one you unzipped).
3. Open **Colony Command**. It shows the pairing screen: enter your PC's VPN
   address and the 8-digit code from **Paired devices, Pair a device** on the PC.

### Build it yourself

You need the Android SDK and NDK (Android Studio installs them), JDK 17 or newer, the
Rust Android target, and these environment variables: `ANDROID_HOME`, `NDK_HOME`,
`JAVA_HOME`.

    rustup target add aarch64-linux-android
    cd app
    npm ci
    npx tauri android init
    ..\scripts\android-prepare.sh      # Git Bash on Windows, or sh on Linux/macOS
    npx tauri android build --debug --apk --target aarch64

`android-prepare.sh` adds what the generated project lacks: the network security
config and the `INTERNET` permission. `gen/android` is generated, so it is not
committed; run `init` again on a fresh checkout.

### Why cleartext is allowed

Colony connects with plain `ws://` over the VPN, which already encrypts the traffic
(the reasoning is in `docs/remote-access.md`). Android blocks cleartext by default
and its configuration can name hosts but not address ranges, so
`app/src-tauri/android/network_security_config.xml` allows it for the app. The app only
ever connects to the one address you paired with. If that bothers you, use a MagicDNS
name and narrow the file to `ts.net` with a `<domain-config>`.

### What a phone or tablet does not get

Some desktop features act on the PC itself. They are hidden on a phone or tablet, not
offered behind a higher scope. `platform().has` is the one switch, and the desktop is
the only place that has all of it.

| Feature | On a phone or tablet | Why |
| --- | --- | --- |
| Setup wizard (hooks, WSL probe, autostart) | Hidden | Installs and removes things on the PC. |
| Autostart | Hidden | A setting of the PC's own login. |
| Approvals gating pause (tray toggle) | Hidden | It switches the PC's hook gating. No remote command exists for it; adding one would need control scope and its own entry in the command tables. |
| Updater | Hidden | The phone app updates through its package, not the desktop updater. |
| Desktop patience notifications | Hidden | They are the PC's own OS notifications. |
| Restarting the daemon | Hidden | The daemon on the PC is not the app's to restart. |
| Pairing and managing devices | Hidden on a paired device | Desktop only, whatever scope a device has. |

The remaining panels follow the same rule: what only reads is shown to every device, and
what changes something needs the control scope.

| Panel | View-only device | Control device |
| --- | --- | --- |
| Time-lapse, spend and latency charts | Shown | Shown |
| Settings and budgets | Values shown | Can change them |
| Session actions (pause, resume, end, kill, compact) | Hidden | Shown |
| Terminal | Can watch | Types, key bar, interrupt |

### What is not there yet

- The app has been built and unit tested, but a run on a physical Android device is
  still to do.
- The paired credential is kept in the webview's local storage, which other apps
  cannot read but a rooted device can. Moving it into the Android Keystore is a
  follow-up.
- Notifications on the device are not wired up; the PC still sends its own.
