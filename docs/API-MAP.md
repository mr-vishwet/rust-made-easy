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
| `remote` / `@electron/remote` | — | — | MANUAL | No Tauri equivalent; every remote call becomes an explicit `invoke()` + Rust command. |
| `autoUpdater` | `tauri-plugin-updater` | `tauri-plugin-updater` | AUTO | Update endpoints and signatures map; the updater needs a signing key configured by the developer. |
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

## Coverage summary

- **AUTO:** 24 entries — the core app, window, IPC, dialog, shell, notification, shortcut,
  updater, clipboard, and bridge surface. A typical Electron app's main process is mostly
  these.
- **ASSISTED:** 9 entries — menus, tray, protocol, keychain, theme, networking, images, and
  multi-monitor. Converts with a named human decision each.
- **MANUAL:** 5 entries — session/cookies, power monitor, crash reporter, and the two native
  modules. Honest gaps, each with a named replacement or reason.

These counts describe the map, not any particular app: run `e2r scan` to see your app's own
breakdown.

## Contributing a mapping

New mappings are the easiest high-value contribution. Add the JSON entry, the codegen template
(for AUTO), the row here, and a fixture — see [CONTRIBUTING.md](../CONTRIBUTING.md).
