import { useState } from "react";
import type { Profile } from "../lib/types";
import { activateProfile, errorMessage } from "../lib/tauri";
import { t } from "../i18n/index.ts";

interface ProfileGateProps {
  profiles: Profile[];
  initialProfileId: number | null;
  onUnlocked: () => void | Promise<void>;
}

export function ProfileGate({ profiles, initialProfileId, onUnlocked }: ProfileGateProps) {
  const [selectedId, setSelectedId] = useState<number | null>(() => {
    if (profiles.some((profile) => profile.id === initialProfileId)) return initialProfileId;
    return profiles.length > 0 ? profiles[0].id : null;
  });
  const [pin, setPin] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const selected = profiles.find((profile) => profile.id === selectedId) || null;
  const locked = Boolean(selected && selected.has_pin);

  async function unlock(profile: Profile, value?: string) {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await activateProfile(profile.id, value);
      setPin("");
      await onUnlocked();
    } catch (failure) {
      // A rejected unlock stays locked: clear the field, report it, and keep the user here.
      setPin("");
      setError(errorMessage(failure) || t("profileGate.failed", { name: profile.name }));
    } finally {
      setBusy(false);
    }
  }

  function select(profile: Profile) {
    setError(null);
    setPin("");
    setSelectedId(profile.id);
    if (!profile.has_pin) void unlock(profile);
  }

  return (
    <div
      className="profile-gate"
      role="dialog"
      aria-modal="true"
      aria-labelledby="profile-gate-title"
    >
      <div className="profile-gate-card">
        <p className="eyebrow">{t("profileGate.eyebrow")}</p>
        <h1 className="profile-gate-title" id="profile-gate-title">
          {locked && selected ? t("profileGate.unlock", { name: selected.name }) : t("profileGate.choose")}
        </h1>
        <p className="profile-gate-sub">
          {t("profileGate.subtitle")}
        </p>

        {profiles.length === 0 ? (
          <p className="profile-gate-sub">{t("profileGate.none")}</p>
        ) : (
          <div className="profile-tiles">
            {profiles.map((profile) => (
              <button
                key={profile.id}
                type="button"
                className={profile.id === selectedId ? "profile-tile active" : "profile-tile"}
                aria-pressed={profile.id === selectedId}
                disabled={busy}
                onClick={() => select(profile)}
              >
                <span className="profile-avatar" aria-hidden="true">
                  {profile.name.slice(0, 1).toUpperCase()}
                </span>
                <span className="profile-tile-name">
                  {profile.has_pin ? t("profileGate.tilePin", { name: profile.name }) : profile.name}
                </span>
              </button>
            ))}
          </div>
        )}

        {locked && selected && (
          <form
            className="profile-pin"
            onSubmit={(event) => {
              event.preventDefault();
              if (pin.length > 0) void unlock(selected, pin);
            }}
          >
            <label htmlFor="profile-pin-input">{t("profileGate.enterPin", { name: selected.name })}</label>
            <div className="profile-pin-row">
              <input
                id="profile-pin-input"
                className="profile-pin-input"
                type="password"
                inputMode="numeric"
                autoComplete="off"
                aria-describedby="profile-pin-hint"
                value={pin}
                autoFocus
                disabled={busy}
                onChange={(event) => setPin(event.target.value)}
              />
              <button className="primary-button" type="submit" disabled={busy || pin.length === 0}>
                {busy ? t("profileGate.unlocking") : t("profileGate.unlockButton")}
              </button>
            </div>
            <small id="profile-pin-hint" className="muted-note">{t("profileGate.pinHint")}</small>
          </form>
        )}

        {error && (
          <p className="profile-gate-error" role="alert">
            {error}
          </p>
        )}

        <p className="profile-gate-note">
          {t("profileGate.note")}
        </p>
      </div>
    </div>
  );
}
