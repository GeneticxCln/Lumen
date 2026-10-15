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
import { Icon } from "./Icon";

interface ProfileSettingsProps {
  profiles: Profile[];
  activeProfileId: number | null;
  onProfilesChanged: () => void | Promise<void>;
}

/** Which inline form a row currently has open, if any. */
type PendingAction = { profileId: number; kind: "switch" | "rename" | "pin" };

const MIN_PIN_CHARS = 4;

function initials(name: string): string {
  const first = name.trim().charAt(0);
  return first ? first.toUpperCase() : "?";
}

export function ProfileSettings({ profiles, activeProfileId, onProfilesChanged }: ProfileSettingsProps) {
  const [pending, setPending] = useState<PendingAction | null>(null);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<number | null>(null);
  const [newName, setNewName] = useState("");

  const fail = (failure: unknown) => setError(errorMessage(failure));

  const openAction = (profile: Profile, kind: PendingAction["kind"]) => {
    setPending({ profileId: profile.id, kind });
    setValue(kind === "rename" ? profile.name : "");
    setError(null);
    setConfirmDeleteId(null);
  };

  const closeAction = () => {
    setPending(null);
    setValue("");
  };

  /** Run one profile mutation, then let the caller reload profiles, active profile and library. */
  const run = async (task: () => Promise<unknown>, after?: () => void) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await task();
      after?.();
      await onProfilesChanged();
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
        setError("A profile needs a name.");
        return;
      }
      void run(() => renameProfile(profile.id, trimmed), closeAction);
      return;
    }

    // Switching to a profile that has a PIN unlocks it, so the PIN travels with the switch.
    void run(() => activateProfile(profile.id, value), closeAction);
  };

  const switchTo = (profile: Profile) => {
    if (profile.has_pin) {
      openAction(profile, "switch");
      return;
    }
    void run(() => activateProfile(profile.id), closeAction);
  };

  const setPin = (profile: Profile) => {
    if (value.length < MIN_PIN_CHARS) {
      setError(`A PIN needs at least ${MIN_PIN_CHARS} characters.`);
      return;
    }
    void run(() => setProfilePin(profile.id, value), closeAction);
  };

  const addProfile = () => {
    const name = newName.trim();
    if (!name) {
      setError("A profile needs a name.");
      return;
    }
    void run(() => createProfile(name), () => setNewName(""));
  };

  return (
    <section className="profile-settings">
      {profiles.length === 0 ? (
        <p className="muted-note">
          Profiles need the desktop app. This browser development preview has no profile API, so there is
          nothing to manage here.
        </p>
      ) : (
        <div className="profile-list">
          {profiles.map((profile) => {
            const isCurrent = profile.id === activeProfileId;
            const isPending = pending !== null && pending.profileId === profile.id;
            const pinOpen = isPending && pending.kind === "pin";
            const switchOpen = isPending && pending.kind === "switch";

            return (
              <div className="profile-row" key={profile.id}>
                <span className="profile-avatar" aria-hidden="true">{initials(profile.name)}</span>
                <div className="profile-row-main">
                  <h3>{profile.name}</h3>
                  <p>
                    {profile.has_pin ? "Locked with a PIN" : "No PIN"} ·{" "}
                    {isCurrent ? "Watch history and My List are saved here." : "Separate history and My List."}
                  </p>
                </div>
                {isCurrent && <span className="profile-badge current">Watching</span>}
                {profile.has_pin && <span className="profile-badge">PIN</span>}

                <div className="profile-row-actions">
                  {!isCurrent && !switchOpen && (
                    <button disabled={busy} onClick={() => switchTo(profile)}>Switch to</button>
                  )}
                  {!isPending && (
                    <button disabled={busy} onClick={() => openAction(profile, "rename")}>Rename</button>
                  )}
                  {!isPending && !profile.has_pin && (
                    <button disabled={busy} onClick={() => openAction(profile, "pin")}>Set PIN</button>
                  )}
                  {!isPending && profile.has_pin && (
                    <>
                      <button disabled={busy} onClick={() => openAction(profile, "pin")}>Change PIN</button>
                      <button disabled={busy} onClick={() => void run(() => setProfilePin(profile.id))}>
                        Remove PIN
                      </button>
                    </>
                  )}
                  {confirmDeleteId !== profile.id ? (
                    <button
                      className="danger"
                      disabled={busy || profiles.length < 2}
                      title={profiles.length < 2 ? "The only profile cannot be deleted" : undefined}
                      onClick={() => {
                        setConfirmDeleteId(profile.id);
                        setPending(null);
                        setError(null);
                      }}
                    >
                      Delete
                    </button>
                  ) : (
                    <>
                      <button
                        className="danger"
                        disabled={busy}
                        onClick={() => void run(() => deleteProfile(profile.id), () => setConfirmDeleteId(null))}
                      >
                        Confirm delete
                      </button>
                      <button disabled={busy} onClick={() => setConfirmDeleteId(null)}>Cancel</button>
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
                    <label className="sr-only" htmlFor={`profile-rename-${profile.id}`}>New name</label>
                    <input
                      id={`profile-rename-${profile.id}`}
                      value={value}
                      maxLength={40}
                      disabled={busy}
                      onChange={(event) => setValue(event.target.value)}
                    />
                    <button className="primary-button" type="submit" disabled={busy}>Save name</button>
                    <button className="secondary-button" type="button" disabled={busy} onClick={closeAction}>
                      Cancel
                    </button>
                  </form>
                )}

                {(switchOpen || pinOpen) && (
                  <form
                    className="profile-pin-form"
                    onSubmit={(event) => {
                      event.preventDefault();
                      if (pinOpen) setPin(profile);
                      else submitPending(profile);
                    }}
                  >
                    <label className="sr-only" htmlFor={`profile-pin-${profile.id}`}>
                      {switchOpen ? `PIN for ${profile.name}` : `New PIN for ${profile.name}`}
                    </label>
                    <input
                      id={`profile-pin-${profile.id}`}
                      type="password"
                      inputMode="numeric"
                      autoComplete="off"
                      value={value}
                      disabled={busy}
                      onChange={(event) => setValue(event.target.value)}
                    />
                    <small>{pinOpen ? `At least ${MIN_PIN_CHARS} characters.` : "Enter this profile's PIN."}</small>
                    <button className="primary-button" type="submit" disabled={busy}>
                      {pinOpen ? "Save PIN" : "Unlock"}
                    </button>
                    <button className="secondary-button" type="button" disabled={busy} onClick={closeAction}>
                      Cancel
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
        <label className="sr-only" htmlFor="profile-new-name">New profile name</label>
        <input
          id="profile-new-name"
          placeholder="Add a profile"
          value={newName}
          maxLength={40}
          disabled={busy}
          onChange={(event) => setNewName(event.target.value)}
        />
        <button className="primary-button" type="submit" disabled={busy}>
          <Icon name="plus" size={17} />Add profile
        </button>
      </form>}

      <p className="profile-inline-note">
        Deleting a profile removes its watch history and My List. The library of sources and its channels is
        shared, so it stays.
      </p>
      <p className="settings-note">
        <strong>A PIN locks one profile on this device.</strong> It is not encryption: the library, the
        database and the media files are still readable by anyone with access to this machine.
      </p>
    </section>
  );
}
