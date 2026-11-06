const { app, BrowserWindow } = require("electron");

app.whenReady().then(() => {
  const win = new BrowserWindow({ x: 120, y: 80, width: 1000, height: 760, frame: false, backgroundColor: "#121212" });
  win.loadFile(process.argv[process.argv.length - 1]);
});
