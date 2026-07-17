/* @refresh reload */
import { render } from "solid-js/web";

import App from "./App";
import CaptureApp from "./CaptureApp";
import { initLocaleFromEnvironment } from "./i18n";
import "./styles.css";

initLocaleFromEnvironment();

const root = document.getElementById("root");
if (!root) throw new Error("missing #root element");

const isCapture = new URLSearchParams(location.search).has("capture");
render(() => (isCapture ? <CaptureApp /> : <App />), root);
