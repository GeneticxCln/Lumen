import { useState } from "react";
import type { Profile } from "../lib/types";
import { activateProfile, errorMessage } from "../lib/tauri";

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
      setError(errorMessage(failure) || `Could not unlock ${profile.name}.`);
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
        <p className="eyebrow">WHO&rsquo;S WATCHING</p>
        <h1 className="profile-gate-title" id="profile-gate-title">
          {locked && selected ? `Unlock ${selected.name}` : "Choose a profile"}
        </h1>
        <p className="profile-gate-sub">
          Watch history and My List are kept separately for each profile on this device.
        </p>

        {profiles.length === 0 ? (
          <p className="profile-gate-sub">No profiles are available on this device.</p>
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
                  {profile.has_pin ? `${profile.name} \u00b7 PIN` : profile.name}
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
            <label htmlFor="profile-pin-input">Enter {selected.name}&rsquo;s PIN</label>
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
                {busy ? "Unlocking…" : "Unlock"}
              </button>
            </div>
            <small id="profile-pin-hint" className="muted-note">At least 4 characters.</small>
          </form>
        )}

        {error && (
          <p className="profile-gate-error" role="alert">
            {error}
          </p>
        )}

        <p className="profile-gate-note">
          The PIN locks this profile&rsquo;s screen on this device. The media library, the database
          and your files are not encrypted by it.
        </p>
      </div>
    </div>
  );
}
