const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('api', {
  checkForUpdates: () => ipcRenderer.invoke('check-for-updates'),
  openExternal: (url) => ipcRenderer.invoke('open-external', url),
  showNotification: (title, body) =>
    ipcRenderer.invoke('show-notification', { title, body }),
  saveSettings: (settings) => ipcRenderer.invoke('save-settings', settings),
});

// A second preload key, so the generated bridge has to emit a generic
// proxy for it (the `versions` pattern from Electron's own docs).
contextBridge.exposeInMainWorld('versions', {
  node: () => process.versions.node,
  chrome: () => process.versions.chrome,
  electron: () => process.versions.electron,
});
