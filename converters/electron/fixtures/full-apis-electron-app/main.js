const { app, BrowserWindow, Tray, Menu, shell, Notification, ipcMain, dialog, nativeTheme, globalShortcut, clipboard } = require('electron');
const path = require('node:path');
const { autoUpdater } = require('electron-updater');
const Store = require('electron-store');

const store = new Store();

let mainWindow = null;
let tray = null;

// The tray menu is a STATIC literal template: one item opens an external
// link, one triggers an update check.
const trayMenu = Menu.buildFromTemplate([
  {
    label: 'Open Documentation',
    click: async () => {
      await shell.openExternal('https://github.com/mr-vishwet/rust-made-easy');
    },
  },
  {
    label: 'Check for Updates',
    click: () => {
      autoUpdater.checkForUpdatesAndNotify();
    },
  },
  { type: 'separator' },
  {
    label: 'Show',
    click: () => {
      if (mainWindow) {
        mainWindow.show();
      }
    },
  },
  { label: 'Quit', role: 'quit' },
]);

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 800,
    height: 600,
    title: 'Full APIs App',
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
    },
  });
  mainWindow.loadFile('index.html');
}

function createTray() {
  tray = new Tray(path.join(__dirname, 'icon.png'));
  tray.setContextMenu(trayMenu);
  tray.setToolTip('Full APIs App');
}

function showNotification(title, body, onClick) {
  const notification = new Notification({ title, body });
  if (onClick) {
    notification.on('click', onClick);
  }
  notification.show();
}

// Canonical electron-updater flow: notify when an update is available,
// and when it has downloaded, ask before restarting to install it.
autoUpdater.on('update-available', (info) => {
  showNotification(
    'Update available',
    `Version ${info.version} is downloading in the background.`,
    () => {
      if (mainWindow) {
        mainWindow.show();
      }
    },
  );
});

autoUpdater.on('update-downloaded', async () => {
  const { response } = await dialog.showMessageBox(mainWindow, {
    type: 'info',
    buttons: ['Restart now', 'Later'],
    title: 'Update ready',
    message: 'A new version has downloaded. Restart the app to install it?',
  });
  if (response === 0) {
    autoUpdater.quitAndInstall();
  }
});

ipcMain.handle('check-for-updates', async () => {
  const result = await autoUpdater.checkForUpdates();
  return result ? result.updateInfo.version : null;
});

ipcMain.handle('open-external', async (_event, url) => {
  await shell.openExternal(url);
  return true;
});

ipcMain.handle('show-notification', async (_event, { title, body }) => {
  showNotification(title, body, () => {
    if (mainWindow) {
      mainWindow.show();
    }
  });
  return true;
});

ipcMain.handle('save-settings', async (_event, settings) => {
  store.set('settings', settings);
  return true;
});

app.whenReady().then(() => {
  Menu.setApplicationMenu(trayMenu);
  createWindow();
  createTray();
  autoUpdater.checkForUpdatesAndNotify();

  // Theme, global shortcut, and clipboard: the fixture exercises these
  // three APIs so the generated `set_theme` command and the
  // global-shortcut / clipboard-manager plugin inits are proven to compile.
  nativeTheme.themeSource = 'dark';
  globalShortcut.register('CommandOrControl+Shift+X', () => {
    clipboard.writeText('Full APIs App');
  });

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow();
    }
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') {
    app.quit();
  }
});
