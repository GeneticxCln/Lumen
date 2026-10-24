import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";
import { Icon } from "./Icon";
import { frontendCrashLog } from "../lib/tauri";
import { t } from "../i18n/index.ts";

/** Reports to the same log file as backend crashes, so a crash with a blank window still leaves
 * a record. Used both by this boundary's `componentDidCatch` and by the `window.onerror`/
 * `unhandledrejection` handlers wired up in `main.tsx`, which an error boundary alone can't catch. */
export function reportCrash(message: string, stack?: string) {
  frontendCrashLog(message, stack).catch(() => {
    // Logging the crash failed too; nothing more we can do about it from here.
  });
}

export class ErrorBoundary extends Component<{ children: ReactNode }, { hasError: boolean }> {
  state = { hasError: false };

  static getDerivedStateFromError() {
    return { hasError: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    reportCrash(error.message, error.stack ?? info.componentStack ?? undefined);
  }

  render() {
    if (!this.state.hasError) return this.props.children;
    return (
      <div className="empty-state">
        <div className="empty-symbol">
          <Icon name="info" size={30} />
        </div>
        <h2>{t("errorBoundary.title")}</h2>
        <p>{t("errorBoundary.body")}</p>
        <button className="primary-button" onClick={() => window.location.reload()}>
          {t("errorBoundary.reload")}
        </button>
      </div>
    );
  }
}
