import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyTheme, readPreferences } from "./lib/discovery";
import { initLocale } from "./i18n/locale";

initLocale();
applyTheme(readPreferences().theme);

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
