import { useState } from "react";
import {
  activateProfile,
  createProfile,
  deleteProfile,
  errorMessage,
  renameProfile,
  setProfilePin,
} from "../lib/tauri";
import type { Profile } from "../lib/types";
import { Rich } from "../i18n/Rich.tsx";
import { t } from "../i18n/index.ts";
import { Icon } from "./Icon";

interface ProfileSettingsProps {
  profiles: Profile[];
  activeProfileId: number | null;
  /** Told which profile a PIN (or the lack of one) was just verified for, when a switch did so. */
  onProfilesChanged: (verifiedProfileId?: number) => void | Promise<void>;
}

/** Which inline form a row currently has open, if any. */
type PendingAction = { profileId: number; kind: "switch" | "rename" | "pin" | "removePin" };

const MIN_PIN_CHARS = 4;

function initials(name: string): string {
  const first = name.trim().charAt(0);
  return first ? first.toUpperCase() : "?";
}

export function ProfileSettings({ profiles, activeProfileId, onProfilesChanged }: ProfileSettingsProps) {
  const [pending, setPending] = useState<PendingAction | null>(null);
  const [value, setValue] = useState("");
  // A profile with a PIN needs that PIN again for any change to it (rename, new PIN, remove, delete).
  const [currentPin, setCurrentPin] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<number | null>(null);
  const [newName, setNewName] = useState("");

  const fail = (failure: unknown) => setError(errorMessage(failure));

  const openAction = (profile: Profile, kind: PendingAction["kind"]) => {
    setPending({ profileId: profile.id, kind });
    setValue(kind === "rename" ? profile.name : "");
    setCurrentPin("");
    setError(null);
    setConfirmDeleteId(null);
  };

  const closeAction = () => {
    setPending(null);
    setValue("");
    setCurrentPin("");
  };

  /** Run one profile mutation, then let the caller reload profiles, active profile and library.
   * `verifiedProfileId` is set only when `task` itself just checked a PIN (or confirmed none is
   * needed) to switch to that profile, so the caller can treat it as already unlocked. */
  const run = async (task: () => Promise<unknown>, after?: () => void, verifiedProfileId?: number) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await task();
      after?.();
      await onProfilesChanged(verifiedProfileId);
    } catch (failure) {
      fail(failure);
    } finally {
      setBusy(false);
    }
  };

  const submitPending = (profile: Profile) => {
    if (!pending || pending.profileId !== profile.id) return;

    if (pending.kind === "rename") {
      const trimmed = value.trim();
      if (!trimmed) {
        setError(t("profiles.needsName"));
        return;
      }
      void run(() => renameProfile(profile.id, trimmed, currentPin), closeAction);
      return;
    }

    if (pending.kind === "removePin") {
      void run(() => setProfilePin(profile.id, undefined, currentPin), closeAction);
      return;
    }

    // Switching to a profile that has a PIN unlocks it, so the PIN travels with the switch.
    void run(() => activateProfile(profile.id, value), closeAction, profile.id);
  };

  const switchTo = (profile: Profile) => {
    if (profile.has_pin) {
      openAction(profile, "switch");
      return;
    }
    void run(() => activateProfile(profile.id), closeAction, profile.id);
  };

  const setPin = (profile: Profile) => {
    if (value.length < MIN_PIN_CHARS) {
      setError(t("profiles.pinTooShort", { min: MIN_PIN_CHARS }));
      return;
    }
    void run(() => setProfilePin(profile.id, value, currentPin), closeAction);
  };

  const addProfile = () => {
    const name = newName.trim();
    if (!name) {
      setError(t("profiles.needsName"));
      return;
    }
    void run(() => createProfile(name), () => setNewName(""));
  };

  return (
    <section className="profile-settings">
      {profiles.length === 0 ? (
        <p className="muted-note">{t("profiles.loadFailed")}</p>
      ) : (
        <div className="profile-list">
          {profiles.map((profile) => {
            const isCurrent = profile.id === activeProfileId;
            const isPending = pending !== null && pending.profileId === profile.id;
            const pinOpen = isPending && pending.kind === "pin";
            const switchOpen = isPending && pending.kind === "switch";
            const removeOpen = isPending && pending.kind === "removePin";

            return (
              <div className="profile-row" key={profile.id}>
                <span className="profile-avatar" aria-hidden="true">{initials(profile.name)}</span>
                <div className="profile-row-main">
                  <h3>{profile.name}</h3>
                  <p>
                    {t(profile.has_pin
                      ? isCurrent ? "profiles.summary.pinCurrent" : "profiles.summary.pinOther"
                      : isCurrent ? "profiles.summary.noPinCurrent" : "profiles.summary.noPinOther")}
                  </p>
                </div>
                {isCurrent && <span className="profile-badge current">{t("profiles.badge.watching")}</span>}
                {profile.has_pin && <span className="profile-badge">{t("profiles.badge.pin")}</span>}

                <div className="profile-row-actions">
                  {!isCurrent && !switchOpen && (
                    <button disabled={busy} onClick={() => switchTo(profile)}>{t("profiles.switchTo")}</button>
                  )}
                  {!isPending && (
                    <button disabled={busy} onClick={() => openAction(profile, "rename")}>{t("profiles.rename")}</button>
                  )}
                  {!isPending && !profile.has_pin && (
                    <button disabled={busy} onClick={() => openAction(profile, "pin")}>{t("profiles.setPin")}</button>
                  )}
                  {!isPending && profile.has_pin && (
                    <>
                      <button disabled={busy} onClick={() => openAction(profile, "pin")}>{t("profiles.changePin")}</button>
                      <button disabled={busy} onClick={() => openAction(profile, "removePin")}>
                        {t("profiles.removePin")}
                      </button>
                    </>
                  )}
                  {confirmDeleteId !== profile.id ? (
                    <button
                      className="danger"
                      disabled={busy || profiles.length < 2}
                      title={profiles.length < 2 ? t("profiles.deleteOnlyOne") : undefined}
                      onClick={() => {
                        setConfirmDeleteId(profile.id);
                        setPending(null);
                        setCurrentPin("");
                        setError(null);
                      }}
                    >
                      {t("profiles.delete")}
                    </button>
                  ) : (
                    <>
                      {profile.has_pin && (
                        <input
                          type="password"
                          inputMode="numeric"
                          autoComplete="off"
                          aria-label={t("profiles.currentPinFor", { name: profile.name })}
                          placeholder={t("profiles.currentPin")}
                          value={currentPin}
                          disabled={busy}
                          onChange={(event) => setCurrentPin(event.target.value)}
                        />
                      )}
                      <button
                        className="danger"
                        disabled={busy}
                        onClick={() => void run(() => deleteProfile(profile.id, currentPin), () => { setConfirmDeleteId(null); setCurrentPin(""); })}
                      >
                        {t("profiles.confirmDelete")}
                      </button>
                      <button disabled={busy} onClick={() => setConfirmDeleteId(null)}>{t("common.cancel")}</button>
                    </>
                  )}
                </div>

                {isPending && pending.kind === "rename" && (
                  <form
                    className="profile-pin-form"
                    onSubmit={(event) => {
                      event.preventDefault();
                      submitPending(profile);
                    }}
                  >
                    {profile.has_pin && (
                      <input
                        type="password"
                        inputMode="numeric"
                        autoComplete="off"
                        aria-label={t("profiles.currentPinFor", { name: profile.name })}
                        placeholder={t("profiles.currentPin")}
                        value={currentPin}
                        disabled={busy}
                        onChange={(event) => setCurrentPin(event.target.value)}
                      />
                    )}
                    <label className="sr-only" htmlFor={`profile-rename-${profile.id}`}>{t("profiles.newName")}</label>
                    <input
                      id={`profile-rename-${profile.id}`}
                      value={value}
                      maxLength={40}
                      disabled={busy}
                      onChange={(event) => setValue(event.target.value)}
                    />
                    <button className="primary-button" type="submit" disabled={busy}>{t("profiles.saveName")}</button>
                    <button className="secondary-button" type="button" disabled={busy} onClick={closeAction}>
                      {t("common.cancel")}
                    </button>
                  </form>
                )}

                {(switchOpen || pinOpen || removeOpen) && (
                  <form
                    className="profile-pin-form"
                    onSubmit={(event) => {
                      event.preventDefault();
                      if (pinOpen) setPin(profile);
                      else submitPending(profile);
                    }}
                  >
                    {((pinOpen && profile.has_pin) || removeOpen) && (
                      <input
                        type="password"
                        inputMode="numeric"
                        autoComplete="off"
                        aria-label={t("profiles.currentPinFor", { name: profile.name })}
                        placeholder={t("profiles.currentPin")}
                        value={currentPin}
                        disabled={busy}
                        onChange={(event) => setCurrentPin(event.target.value)}
                      />
                    )}
                    {!removeOpen && (
                      <>
                        <label className="sr-only" htmlFor={`profile-pin-${profile.id}`}>
                          {switchOpen ? t("profiles.pinFor", { name: profile.name }) : t("profiles.newPinFor", { name: profile.name })}
                        </label>
                        <input
                          id={`profile-pin-${profile.id}`}
                          type="password"
                          inputMode="numeric"
                          autoComplete="off"
                          placeholder={pinOpen ? t("profiles.newPin") : undefined}
                          value={value}
                          disabled={busy}
                          onChange={(event) => setValue(event.target.value)}
                        />
                        <small>{pinOpen ? t("profiles.pinMinHint", { min: MIN_PIN_CHARS }) : t("profiles.enterPin")}</small>
                      </>
                    )}
                    <button className="primary-button" type="submit" disabled={busy}>
                      {pinOpen ? t("profiles.savePin") : removeOpen ? t("profiles.removePin") : t("profileGate.unlockButton")}
                    </button>
                    <button className="secondary-button" type="button" disabled={busy} onClick={closeAction}>
                      {t("common.cancel")}
                    </button>
                  </form>
                )}
              </div>
            );
          })}
        </div>
      )}

      {error && (
        <p className="profile-inline-error" role="alert">
          {error}
        </p>
      )}

      {/* An empty list means the profile API is unavailable, not that no profile exists:
          the migration always seeds one. So the create form is only offered when it works. */}
      {profiles.length > 0 && <form
        className="profile-create"
        onSubmit={(event) => {
          event.preventDefault();
          addProfile();
        }}
      >
        <label className="sr-only" htmlFor="profile-new-name">{t("profiles.newProfileName")}</label>
        <input
          id="profile-new-name"
          placeholder={t("profiles.addPlaceholder")}
          value={newName}
          maxLength={40}
          disabled={busy}
          onChange={(event) => setNewName(event.target.value)}
        />
        <button className="primary-button" type="submit" disabled={busy}>
          <Icon name="plus" size={17} />{t("profiles.add")}
        </button>
      </form>}

      <p className="profile-inline-note">
        {t("profiles.deleteNote")}
      </p>
      <p className="settings-note">
        <Rich id="profiles.pinNote" tags={{ strong: (text) => <strong>{text}</strong> }} />
      </p>
    </section>
  );
}
