/* @refresh reload */
import { render } from "solid-js/web";

import App from "./App";
import { initLocaleFromEnvironment } from "./i18n";
import "./styles.css";

initLocaleFromEnvironment();

const root = document.getElementById("root");
if (!root) throw new Error("missing #root element");

render(() => <App />, root);
