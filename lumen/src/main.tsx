import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyTheme, readPreferences } from "./lib/discovery";

applyTheme(readPreferences().theme);

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
