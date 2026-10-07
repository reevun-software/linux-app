// The shared app (the window showing reevun.app, updates), built from
// reevun-software/app-core into dist/core next to this file - so it's
// loaded from there at run time, and typed from there at build time.
const { startApp } = require("./core/electron/main") as typeof import("../dist/core/electron/main");

// Same window as on Windows: no system frame, the app draws its own title
// strip with minimize / maximize / close buttons.
const TITLE_BAR_HEIGHT = 36;

startApp({
  platform: "linux",
  window: {
    titleBarStyle: "hidden",
  },
  titleBar: { height: TITLE_BAR_HEIGHT, insetLeft: 0, insetRight: 0, windowButtons: true },
});
