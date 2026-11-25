# API Map: Electron → Tauri

> **Source of truth is `converters/electron/mappings/electron-to-tauri.json`; this doc mirrors it.** If the two
> disagree, the JSON wins — and please open an issue, because they shouldn't disagree.

## Statuses

| Status | Meaning |
| --- | --- |
| **AUTO** | Converts mechanically. No human decision needed; the generated code is ready. |
| **ASSISTED** | Converts, but a human must confirm a choice (a permission, an option, a fallback). The generated `MIGRATION-NOTES.md` names the choice. |
| **MANUAL** | No mechanical conversion exists yet. The entry names the Rust replacement or the reason it can't convert, so you get guidance instead of silence. |

## The map

| Electron API | Tauri target | Plugin / crate | Status | Notes |
| --- | --- | --- | --- | --- |
| `app` | `tauri::App` / `AppHandle` | `tauri` | AUTO | Lifecycle (`ready`, `quit`, `before-quit`) maps to Tauri setup + run hooks. |
| `app.getPath(name)` | `app.path().<dir>()` | `tauri` | AUTO | `userData`, `documents`, `downloads`, `temp`, `exe`, `home` all have `path()` equivalents. |
| `app.quit()` | `app.exit(0)` | `tauri` | AUTO | `app.exit(code)` takes an exit code; plain `quit()` maps to `exit(0)`. |
| `BrowserWindow` | `WebviewWindow` | `tauri` | AUTO | Window options map to `WebviewWindowBuilder`. `webPreferences` need review — see notes on preload below. |
| `ipcMain.handle(channel, fn)` | `#[tauri::command] fn` | `tauri` | AUTO | One Rust command per handler; channel name becomes the command name (snake_case). |
| `ipcMain.on(channel, fn)` | `app.listen(channel, fn)` + `emit` | `tauri` | AUTO | Fire-and-forget handlers become `app.listen` stubs in `src-tauri/src/listeners.rs`; the bridge emits to them. |
| `ipcRenderer.invoke(channel, …)` | `invoke(channel, …)` | `@tauri-apps/api/core` | AUTO | Rebound in the generated `src/lib/e2r-bridge.ts`; call sites keep their shape. |
| `ipcRenderer.on(channel, fn)` | `listen(channel, fn)` | `@tauri-apps/api/event` | AUTO | Event listeners rebound to Tauri's event system. |
| `ipcRenderer.send(channel, …)` | `emit(channel, …)` | `@tauri-apps/api/event` | AUTO | Fire-and-forget sends become `emit`. |
| `dialog.showOpenDialog` | `open()` | `tauri-plugin-dialog` | AUTO | Options map 1:1 except some `properties` values; filters carry over. |
| `dialog.showSaveDialog` | `save()` | `tauri-plugin-dialog` | AUTO | Default path and filters map directly. |
| `dialog.showMessageBox` | `message()` / `ask()` | `tauri-plugin-dialog` | AUTO | Button layouts differ slightly per platform; message + title + kind carry over. |
| `Menu` | `tauri::menu` | `tauri` | ASSISTED | Menu structure converts; accelerators and roles need per-item review, and macOS app-menu conventions differ. |
| `Menu.setApplicationMenu` | `app.set_menu()` | `tauri` | ASSISTED | Same as `Menu`: structure converts, platform conventions need a human look. |
| `Tray` | `tauri::tray::TrayIcon` | `tauri` | ASSISTED | Icon + tooltip + context menu convert; per-platform icon sizing and click behavior need review. |
| `shell.openExternal(url)` | `opener::open_url()` | `tauri-plugin-opener` | AUTO | Direct mapping. |
| `shell.openPath(path)` | `opener::open_path()` | `tauri-plugin-opener` | AUTO | Direct mapping. |
| `Notification` | `Notification::builder()` | `tauri-plugin-notification` | AUTO | Title/body/icon map; action buttons need the plugin's permission set. |
| `globalShortcut.register` | `GlobalShortcut` | `tauri-plugin-global-shortcut` | AUTO | Accelerator strings map; registration conflicts are reported at runtime, not conversion time. |
| `protocol.registerFileProtocol` | custom URI scheme | `tauri` (`UriSchemeProtocol`) | ASSISTED | The scheme registration converts; the handler's file-serving logic needs a Rust port of its path resolution. |
| `session` / `ses.cookies` | Webview cookie APIs | — | MANUAL | No direct equivalent: the system webview's cookie handling is thinner than Chromium's `session`. Per-app design needed. |
| `safeStorage` | OS keychain | `tauri-plugin-stronghold` / keyring | ASSISTED | Encryption-to-keychain mapping exists, but key migration (existing encrypted data) needs a per-app plan. |
| `nativeTheme` | `set_theme` command (`window.set_theme()`) | `tauri` | AUTO | `themeSource` maps to a real command (call via `api.setTheme`); `shouldUseDarkColors` listeners need a small manual port. |
| `app.requestSingleInstanceLock` | `tauri-plugin-single-instance` | `tauri-plugin-single-instance` | AUTO | Lock + second-instance argv handling map to the plugin; the focus/restore logic still needs porting. |
| `app.setAsDefaultProtocolClient` | `tauri-plugin-deep-link` | `tauri-plugin-deep-link` | AUTO | Protocol registration maps to the plugin; the open-url / second-instance URL handling still needs porting. |
| `electron-window-state` | `tauri-plugin-window-state` | `tauri-plugin-window-state` | AUTO | Position/size persistence maps to the plugin; `defaultWidth`/`defaultHeight` carry over as the window's initial size. |
| `nodeIntegration: true` | — | — | MANUAL | A Tauri webview has no Node.js runtime; renderer code importing `electron` or Node builtins needs a rewrite against the bridge. |
| `remote` / `@electron/remote` | — | — | MANUAL | No Tauri equivalent; every remote call becomes an explicit `invoke()` + Rust command. A finding forced to MANUAL because the call goes through remote (rather than being MANUAL on its own merits) carries `remote_mediated: true` in `scan.json`, so the two can be told apart. |
| `autoUpdater` | `tauri-plugin-updater` | `tauri-plugin-updater` | AUTO | Update endpoints and signatures map; the updater needs a signing key configured by the developer. |
| `electron-updater` | `tauri-plugin-updater` | `tauri-plugin-updater` | AUTO | A different library from Electron's `autoUpdater`: the npm package `electron-updater` (its `autoUpdater` is reported under this name, never as Electron's `autoUpdater`). Same plugin target; the update feed and signing configuration still need a developer decision. |
| `webContents.send(channel, …)` | `window.emit(channel, …)` | `tauri` | AUTO | Main-to-renderer messages become window events. |
| `webContents.openDevTools()` | `window.open_devtools()` | `tauri` | AUTO | Debug-only; maps directly. |
| `clipboard` | `ClipboardManager` | `tauri-plugin-clipboard-manager` | AUTO | Read/write text and images map directly. |
| `screen.getPrimaryDisplay()` | `Monitor` / `primary_monitor()` | `tauri` | AUTO | Display bounds and scale factor map; multi-monitor enumeration is ASSISTED (ordering differs). |
| `powerMonitor` | — | — | MANUAL | No Tauri equivalent. Suspend/resume/lock events need platform crates or a sidecar; often droppable. |
| `crashReporter` | external crash service | e.g. `sentry` | MANUAL | No built-in equivalent; wire a crash-reporting service instead. The generated notes suggest the integration points. |
| `net` | `reqwest` / `tauri-plugin-http` | `reqwest` | ASSISTED | `net.request` maps to `reqwest` for most uses; proxy and certificate handling need review. |
| `nativeImage` | `tauri::image::Image` | `tauri` | ASSISTED | Creation from path/buffer maps; resize and format conversion need a small image-crate helper. |
| `contextBridge.exposeInMainWorld` | typed `invoke()` bridge | `@tauri-apps/api/core` | AUTO | A sandboxed preload (pure `contextBridge` + `ipcRenderer`) ports verbatim — only the transport is rebound. See the Stage 1 ladder in [RND.md](RND.md). |
| `electron-store` | `tauri-plugin-store` | `tauri-plugin-store` | AUTO | Key/value persistence maps directly; the store file format differs, so existing user data needs a one-time migration step (flagged in the notes). |
| `node-pty` | `portable-pty` | `portable-pty` | MANUAL | Native module; needs a Rust port. Phase 3 will add a tested recipe. |
| `better-sqlite3` | `rusqlite` | `rusqlite` | MANUAL | Native module; needs a Rust port. Phase 3 will add a tested recipe. |
| `app.commandLine` | — | — | MANUAL | Chromium command-line switches have no Tauri equivalent; each switch needs a per-app decision (drop it, or pass it to the webview where the webview supports it). |
| `app.dock` | — | — | MANUAL | The macOS dock API (setMenu, bounce, show/hide) has no Tauri core equivalent; needs platform Rust code. |
| `app.setBadgeCount` | — | — | MANUAL | Dock/taskbar badge counts have no Tauri core equivalent; needs platform Rust code. |
| `app.setAppUserModelId` | — | — | MANUAL | The Windows AppUserModelID has no Tauri core equivalent; set it from Rust platform code or the Windows app manifest. |
| `app.setJumpList` | — | — | MANUAL | Windows jump lists have no Tauri core equivalent; needs platform Rust code. |
| `app.enableSandbox` | — | — | MANUAL | Enabling Chromium's sandbox has no Tauri equivalent — Tauri webviews are sandboxed by the platform webview, so the call is usually droppable, but verify per app. |
| `app.isPackaged` | — | — | MANUAL | The packaged-vs-dev check has no Tauri equivalent; use the Tauri dev/release build distinction (`tauri::is_dev()` / `cfg(debug_assertions)`) instead. |
| `app.runningUnderARM` | — | — | MANUAL | The ARM-translation check has no Tauri equivalent; use `std::env::consts::ARCH` in Rust instead. |
| `app.setLoginItemSettings` | `tauri-plugin-autostart` | `tauri-plugin-autostart` | MANUAL | Login-item (start at login) settings have no Tauri core equivalent; needs platform Rust code or the autostart plugin. |
| `app.getLoginItemSettings` | `tauri-plugin-autostart` | `tauri-plugin-autostart` | MANUAL | Reading login-item settings has no Tauri core equivalent; use the autostart plugin's query API. |
| `app.setName` | — | — | MANUAL | Renaming the app at runtime has no Tauri equivalent; the app name comes from `tauri.conf.json` instead. |
| `app.setPath` | — | — | MANUAL | Redirecting a standard path (e.g. `userData`) has no Tauri equivalent; the path API does not support overriding the app-data directory. |
| `app.moveToApplicationsFolder` | — | — | MANUAL | Moving the app into /Applications (macOS) has no Tauri equivalent; needs platform Rust code. |
| `app.isInApplicationsFolder` | — | — | MANUAL | Checking whether the app lives in /Applications (macOS) has no Tauri equivalent; needs platform Rust code. |
| `app.disableHardwareAcceleration` | — | — | MANUAL | Disabling Chromium hardware acceleration has no Tauri equivalent; the system webview manages its own rendering. |
| `app.getPreferredSystemLanguages` | — | — | MANUAL | Preferred system languages have no Tauri core equivalent; read the OS locale from Rust instead. |
| `app.setDesktopName` | — | — | MANUAL | The Linux desktop-file name has no Tauri equivalent; it comes from the bundle metadata instead. |
| `app.setAppLogsPath` | — | — | MANUAL | Redirecting the app-logs path has no Tauri equivalent; point the logger at the path directly instead. |
| `app.isDefaultProtocolClient` | `tauri-plugin-deep-link` | `tauri-plugin-deep-link` | AUTO | Querying the default protocol handler maps to the deep-link plugin alongside registration; the open-url handling still needs porting. |
| `app.removeAsDefaultProtocolClient` | `tauri-plugin-deep-link` | `tauri-plugin-deep-link` | AUTO | Removing a protocol registration maps to the deep-link plugin alongside registration. |
| `net.resolveHost` | DNS resolver crate (e.g. `hickory-resolver`) | — | ASSISTED | DNS resolution is not HTTP: `reqwest` cannot do it. Use a DNS resolver crate or `std::net::ToSocketAddrs`. |
| `net.isOnline` | connectivity check (DNS probe / `online` crate) | — | ASSISTED | The connectivity check is not an HTTP request: `reqwest` is the wrong shape. Use a connectivity check (e.g. a DNS probe or the `online` crate). |
| `net.fetch` | `reqwest` | — | ASSISTED | Electron's `net.fetch` maps to `reqwest` in Rust, but the streaming API differs. |
| `net.request` | `reqwest` | — | ASSISTED | Electron's `net.request` maps to `reqwest` in Rust, but the streaming API differs. |
| `WebContentsView` | — | — | MANUAL | Embedding a webview inside a window has no Tauri equivalent; it is a redesign (separate `WebviewWindow` or a webview-tag replacement). |
| `webview` | — | — | MANUAL | The `<webview>` tag (and the `webviewTag` webPreference) embeds a guest page inside a window; Tauri has no webview tag — redesign as a separate `WebviewWindow`, or serve the guest content as a route in the same webview. |
| `utilityProcess` | Rust sidecar (`tauri-plugin-shell`) | — | MANUAL | Utility processes have no Tauri equivalent; use a Rust sidecar (shell-plugin sidecar) instead. |
| `MessageChannelMain` | — | — | MANUAL | MessagePort plumbing between main and renderer has no Tauri equivalent; replace with Tauri events/commands. |
| `desktopCapturer` | — | — | MANUAL | Screen/window capture-source enumeration has no Tauri equivalent; needs a platform plugin (screen capture). |
| `systemPreferences` | — | — | MANUAL | OS preferences (macOS defaults, media-access status, accent color, notification subscriptions) have no Tauri equivalent; needs platform Rust code. |
| `webFrame` | Tauri webview API | — | ASSISTED | Renderer-frame controls (zoom level, spell-checker provider) map to the Tauri webview API, but not one-to-one; check the call site. |
| `webUtils` | Tauri drag-drop events (file paths) | — | ASSISTED | File-drop path resolution has no direct Tauri equivalent; Tauri drag-drop events carry file paths instead. |
| `contentTracing` | — | — | MANUAL | Chromium tracing (`startRecording`/`stopRecording`, heap profiling) has no Tauri equivalent. |
| `ipcMain.handleOnce` | `#[tauri::command]` | — | AUTO | Each one-shot handler becomes a Tauri command registered with `invoke_handler`, like `ipcMain.handle`. |
| `MenuItem` | `tauri::menu::MenuItem` | — | ASSISTED | Menu items map over, but Electron roles have no one-to-one Tauri equivalent. |
| `getGlobal` | — | — | MANUAL | Remote-only: reading a main-process global from the renderer has no Tauri equivalent; expose the value through an `invoke()` + Rust command instead. |
| `getCurrentWindow` | — | — | MANUAL | Remote-only: the current `BrowserWindow` from the renderer has no Tauri equivalent; use the Tauri window API in the webview instead. |
| `getCurrentWebContents` | — | — | MANUAL | Remote-only: the current `WebContents` from the renderer has no Tauri equivalent; use the Tauri webview API instead. |
| `screen.screenToDipPoint` | Tauri window / monitor API (`scaleFactor` + manual DIP math) | — | ASSISTED | DIP conversion needs manual math from the monitor's `scaleFactor`; the monitor API gives the scale factor but not the conversion. |
| `screen.screenToDipRect` | Tauri window / monitor API (`scaleFactor` + manual DIP math) | — | ASSISTED | DIP conversion needs manual math from the monitor's `scaleFactor`; the monitor API gives the scale factor but not the conversion. |
| `webContents.forcefullyCrashRenderer` | — | — | MANUAL | Forcing a renderer crash has no Tauri equivalent. |
| `webContents.isCrashed` | — | — | MANUAL | Checking whether the renderer crashed has no Tauri equivalent; the webview's lifecycle events are the closest signal. |
| `keytar` | `keyring` crate | — | MANUAL | Native keychain bindings become `keyring`-crate calls behind Tauri commands. |
| `sqlite-vec` | `rusqlite` + `sqlite-vec` extension | — | MANUAL | The sqlite-vec vector-search extension becomes `rusqlite` with the sqlite-vec extension loaded. |
| `@signalapp/sqlcipher` | `rusqlite` + SQLCipher | — | MANUAL | Signal's SQLCipher addon becomes `rusqlite` with SQLCipher (or `sqlx` with the `sqlcipher` feature) behind Tauri commands. |
| `fs-admin` | — | — | MANUAL | Privileged file operations (admin elevation) have no Tauri equivalent; needs a Rust port with platform elevation. |
| `registry-js` | `winreg` crate | — | MANUAL | Windows registry access becomes the `winreg` crate behind Tauri commands. |
| `node-llama-cpp` | — | — | MANUAL | llama.cpp bindings are a native addon (model loading, GPU/CPU inference); needs a Rust port (e.g. llama.cpp bindings or a sidecar). |
| `sqlite3` | `rusqlite` | — | MANUAL | Async SQLite access becomes `rusqlite` calls behind Tauri commands. |

## Coverage summary

- **AUTO:** 31 entries — the core app, window, IPC, dialog, shell, notification, shortcut,
  updater, clipboard, and bridge surface, plus one-shot handlers and protocol-client queries.
  A typical Electron app's main process is mostly these.
- **ASSISTED:** 20 entries — menus, tray, protocol, keychain, theme, networking, images,
  multi-monitor, plus `net.*` member calls, `webFrame`/`webUtils`, menu items, and DIP
  conversion. Converts with a named human decision each.
- **MANUAL:** 45 entries — session/cookies, power monitor, crash reporter, native modules,
  member-level `app.*` calls with no Tauri equivalent (dock, badge, jump list, sandbox,
  login items), remote-only accessors, `WebContentsView` / `utilityProcess` /
  `desktopCapturer` / `systemPreferences` / `contentTracing`, the `<webview>` tag, and the proof apps' native
  addons (`keytar`, `sqlite-vec`, `@signalapp/sqlcipher`, `fs-admin`, `registry-js`,
  `node-llama-cpp`). Honest gaps, each with a named replacement or reason.

These counts describe the map, not any particular app: run `e2r scan` to see your app's own
breakdown.

## Contributing a mapping

New mappings are the easiest high-value contribution. Add the JSON entry, the codegen template
(for AUTO), the row here, and a fixture — see [CONTRIBUTING.md](../CONTRIBUTING.md).
