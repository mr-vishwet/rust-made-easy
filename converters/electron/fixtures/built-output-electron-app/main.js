const { app, BrowserWindow, ipcMain } = require('electron');
const path = require('node:path');

function createWindow() {
  const win = new BrowserWindow({
    width: 1024,
    height: 768,
    title: 'Built Output App',
  });
  win.loadFile(path.join(__dirname, 'dist', 'index.html'));
}

ipcMain.handle('get-version', async () => {
  return app.getVersion();
});

app.whenReady().then(() => {
  createWindow();
});
