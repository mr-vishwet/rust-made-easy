const { app, BrowserWindow, powerMonitor, session } = require('electron');
powerMonitor.on('suspend', () => {});
const ses = session.defaultSession;
function createWindow() { const w = new BrowserWindow({ width: 800, height: 600 }); }
app.whenReady().then(createWindow);
