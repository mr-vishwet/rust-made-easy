const { app, BrowserWindow, ipcMain, Tray, Menu, dialog } = require('electron');
const path = require('node:path');
const fs = require('node:fs');
const Store = require('electron-store');

const store = new Store();

let mainWindow = null;
let settingsWindow = null;
let tray = null;

// Application menu: a STATIC literal template (File > Quit, Help > About).
const appMenu = Menu.buildFromTemplate([
  {
    label: 'File',
    submenu: [
      {
        label: 'Open…',
        click: async () => {
          const result = await dialog.showOpenDialog(mainWindow, {
            properties: ['openFile'],
          });
          if (!result.canceled) {
            mainWindow.webContents.send('file-opened', result.filePaths[0]);
          }
        },
      },
      { type: 'separator' },
      { label: 'Quit', role: 'quit' },
    ],
  },
  {
    label: 'Help',
    submenu: [{ label: 'About', role: 'about' }],
  },
]);

function createMainWindow() {
  mainWindow = new BrowserWindow({
    width: 1024,
    height: 768,
    title: 'Minimal App',
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
    },
  });
  mainWindow.loadFile('index.html');
}

function createSettingsWindow() {
  settingsWindow = new BrowserWindow({
    width: 600,
    height: 400,
    title: 'Settings',
    frame: false,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
    },
  });
  settingsWindow.loadFile('index.html');
}

function createTray() {
  tray = new Tray(path.join(__dirname, 'icon.png'));
  const contextMenu = Menu.buildFromTemplate([
    { label: 'Show', click: () => mainWindow && mainWindow.show() },
    { label: 'Quit', click: () => app.quit() },
  ]);
  tray.setContextMenu(contextMenu);
  tray.setToolTip('Minimal App');
}

ipcMain.handle('read-file', async (_event, filePath) => {
  return fs.readFileSync(filePath, 'utf8');
});

ipcMain.handle('save-settings', async (_event, settings) => {
  store.set('settings', settings);
  return true;
});

ipcMain.handle('get-version', async () => {
  return app.getVersion();
});

ipcMain.on('log-message', (_event, message) => {
  console.log('[renderer]', message);
});

app.whenReady().then(() => {
  Menu.setApplicationMenu(appMenu);
  createMainWindow();
  createSettingsWindow();
  createTray();

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createMainWindow();
    }
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') {
    app.quit();
  }
});
