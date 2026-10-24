import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary, reportCrash } from "./components/ErrorBoundary";
import { applyTheme, readPreferences } from "./lib/discovery";
import { initLocale } from "./i18n/locale";

initLocale();
applyTheme(readPreferences().theme);

// Catches what an error boundary can't: errors outside a render (a timer callback, an event
// handler) and rejected promises nobody awaited.
window.onerror = (message, source, line, column, error) => {
  reportCrash(error?.message ?? String(message), error?.stack ?? `${source}:${line}:${column}`);
};
window.onunhandledrejection = (event) => {
  const reason = event.reason;
  reportCrash(reason instanceof Error ? reason.message : String(reason), reason instanceof Error ? reason.stack : undefined);
};

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
