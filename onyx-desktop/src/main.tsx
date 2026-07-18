/* @refresh reload */
import { render } from "solid-js/web";

import { api } from "./api";
import App from "./App";
import CaptureApp from "./CaptureApp";
import { initLocaleFromEnvironment } from "./i18n";
import MobileApp from "./mobile/MobileApp";
import "./styles.css";

initLocaleFromEnvironment();

const root = document.getElementById("root");
if (!root) throw new Error("missing #root element");

const isCapture = new URLSearchParams(location.search).has("capture");

/** Mobile gets its own shell; `onyxForceMobile=1` lets desktop dev preview it. */
async function shellIsMobile(): Promise<boolean> {
  if (localStorage.getItem("onyxForceMobile") === "1") return true;
  try {
    return (await api.platformInfo()).mobile;
  } catch {
    // Plain-browser dev server: no Tauri backend to ask.
    return false;
  }
}

void shellIsMobile().then((mobile) => {
  render(() => (isCapture ? <CaptureApp /> : mobile ? <MobileApp /> : <App />), root);
});
