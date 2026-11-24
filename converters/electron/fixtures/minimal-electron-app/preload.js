const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('api', {
  readFile: (filePath) => ipcRenderer.invoke('read-file', filePath),
  saveSettings: (settings) => ipcRenderer.invoke('save-settings', settings),
  getVersion: () => ipcRenderer.invoke('get-version'),
  onLog: (callback) => ipcRenderer.on('log-message', (_event, message) => callback(message)),
});
