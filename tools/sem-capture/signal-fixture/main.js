// Signal-shaped Electron fixture app (see index.html). Run by capture_more.py:
//   <electron binary> --no-sandbox --disable-gpu --ozone-platform=wayland \
//       --force-renderer-accessibility --user-data-dir=<throwaway> <this directory> ?view=chat
// No network, no account: it loads one local page.
const { app, BrowserWindow, Menu } = require('electron');
const path = require('path');

app.commandLine.appendSwitch('ozone-platform', 'wayland');
const view = process.argv.slice(1).reverse().find((a) => /^\?view=/.test(a)) || '?view=conversations';

app.whenReady().then(() => {
  // The window chrome a Signal user has: a menu bar with the usual top-level menus.
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    { label: 'File', submenu: [{ label: 'Preferences' }, { role: 'quit' }] },
    { label: 'Edit', submenu: [{ role: 'undo' }, { role: 'redo' }, { type: 'separator' }, { role: 'cut' }, { role: 'copy' }, { role: 'paste' }] },
    { label: 'View', submenu: [{ role: 'togglefullscreen' }, { role: 'zoomIn' }, { role: 'zoomOut' }] },
    { label: 'Window', submenu: [{ role: 'minimize' }, { role: 'close' }] },
    { label: 'Help', submenu: [{ label: 'Support' }, { label: 'About' }] },
  ]));
  const w = new BrowserWindow({ width: 1100, height: 760, autoHideMenuBar: false, title: 'Signal' });
  w.loadFile(path.join(__dirname, 'index.html'), { search: view.slice(1) });
});
app.on('window-all-closed', () => app.quit());
