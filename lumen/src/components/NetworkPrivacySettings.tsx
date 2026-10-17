import { useEffect, useState } from "react";
import {
  errorMessage,
  networkPrivacyState,
  setNetworkPrivacy,
  testNetworkPrivacy,
} from "../lib/tauri";
import { setNetworkPrivacyEnabled } from "../lib/networkPrivacy";
import { t } from "../i18n/index.ts";
import type { NetworkPrivacyState } from "../lib/types";

/**
 * An optional local relay that Lumen's own traffic (metadata, mpv's playback, yt-dlp's
 * extraction, and — once this is on — remote images) is routed through, forwarding to a proxy
 * the user configures. Off by default.
 */
export function NetworkPrivacySettings() {
  const [state, setState] = useState<NetworkPrivacyState | null>(null);
  const [enabled, setEnabled] = useState(false);
  const [upstream, setUpstream] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [passwordTouched, setPasswordTouched] = useState(false);
  const [testResult, setTestResult] = useState<string | null>(null);
  const [busy, setBusy] = useState<"save" | "test" | null>(null);
  const [error, setError] = useState<string | null>(null);

  const applyState = (next: NetworkPrivacyState) => {
    setState(next);
    setEnabled(next.enabled);
    setUpstream(next.upstream);
    setUsername(next.username);
    setPassword("");
    setPasswordTouched(false);
    setNetworkPrivacyEnabled(next.enabled);
  };

  useEffect(() => {
    let active = true;
    networkPrivacyState()
      .then((next) => { if (active) applyState(next); })
      .catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, []);

  const save = async () => {
    setBusy("save");
    setError(null);
    setTestResult(null);
    try {
      applyState(await setNetworkPrivacy(enabled, upstream, username, passwordTouched ? password : undefined));
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(null);
    }
  };

  const test = async () => {
    setBusy("test");
    setError(null);
    setTestResult(null);
    try {
      const time = await testNetworkPrivacy(upstream, username, passwordTouched ? password : undefined);
      setTestResult(t("privacy.test.ok", { time }));
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(null);
    }
  };

  const dirty = !state
    || enabled !== state.enabled
    || upstream !== state.upstream
    || username !== state.username
    || passwordTouched;

  return (
    <div className="privacy-settings">
      <h3 className="settings-section-title">{t("privacy.title")}</h3>
      {!state && !error && <p className="muted-note">{t("privacy.checking")}</p>}
      <p>{t("privacy.intro")}</p>

      {state && (
        <p className="muted-note" role="status">
          {state.status === "off" && t("privacy.status.off")}
          {state.status === "active" && t("privacy.status.active", { upstream: state.upstream })}
          {state.status === "unavailable" && t("privacy.status.unavailable", { reason: state.error ?? "" })}
        </p>
      )}

      <label className="privacy-toggle">
        <input
          type="checkbox"
          checked={enabled}
          disabled={!state || busy !== null}
          onChange={(event) => setEnabled(event.target.checked)}
        />
        {t("privacy.enabledLabel")}
      </label>

      <label htmlFor="privacy-upstream">{t("privacy.upstream.label")}</label>
      <input
        id="privacy-upstream"
        type="text"
        value={upstream}
        placeholder={t("privacy.upstream.placeholder")}
        autoComplete="off"
        spellCheck={false}
        disabled={!state || busy !== null}
        onChange={(event) => setUpstream(event.target.value)}
      />
      <p className="muted-note">{t("privacy.upstream.hint")}</p>

      <label htmlFor="privacy-username">{t("privacy.username.label")}</label>
      <input
        id="privacy-username"
        type="text"
        value={username}
        autoComplete="off"
        spellCheck={false}
        disabled={!state || busy !== null}
        onChange={(event) => setUsername(event.target.value)}
      />

      <label htmlFor="privacy-password">{t("privacy.password.label")}</label>
      <input
        id="privacy-password"
        type="password"
        value={password}
        placeholder={state?.hasPassword ? t("privacy.password.placeholderSet") : t("privacy.password.placeholderUnset")}
        autoComplete="off"
        disabled={!state || busy !== null}
        onChange={(event) => { setPassword(event.target.value); setPasswordTouched(true); }}
      />

      <p className="muted-note">{t("privacy.localBypass")}</p>
      <p className="muted-note">{t("privacy.scopeNote")}</p>

      {testResult && <p className="muted-note" role="status">{testResult}</p>}
      {error && <div className="dialog-error" role="alert">{error}</div>}

      <div className="settings-actions">
        <button className="primary-button" disabled={!state || busy !== null || !dirty} onClick={() => void save()}>
          {busy === "save" ? t("privacy.saving") : t("privacy.save")}
        </button>
        <button className="secondary-button" disabled={!state || busy !== null || upstream.trim() === ""} onClick={() => void test()}>
          {busy === "test" ? t("privacy.testing") : t("privacy.test")}
        </button>
      </div>
      {passwordTouched && <p className="muted-note">{t("privacy.test.usesTypedPassword")}</p>}
    </div>
  );
}
