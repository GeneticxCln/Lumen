import { useState } from "react";
import { Modal } from "./Modal";
import { errorMessage, mergeProfiles } from "../lib/tauri";
import { t } from "../i18n/index.ts";
import type { Profile } from "../lib/types";

/** A pre-existing local profile paired with a profile that just arrived from the sync group. */
export interface MergeCandidate {
  local: Profile;
  remote: Profile;
}

interface MergeProfilesDialogProps {
  candidates: MergeCandidate[];
  onClose: () => void;
  onMerged: (profiles: Profile[]) => void;
}

/**
 * Shown once, right after joining a sync group, when a profile that already existed on this
 * device is still a separate row alongside one that just arrived from the group. Every profile
 * gets its own random sync key at creation time (see `db::create_profile`), so two devices that
 * each created their own default profile before ever syncing have no way to recognize each
 * other as the same person on their own — this is how the user says so by hand. Declining a pair
 * is a valid answer: a group can genuinely hold distinct members.
 */
export function MergeProfilesDialog({ candidates, onClose, onMerged }: MergeProfilesDialogProps) {
  const [remaining, setRemaining] = useState(candidates);
  const [busyId, setBusyId] = useState<number | null>(null);
  const [errors, setErrors] = useState<Record<number, string>>({});

  const dismiss = (remoteId: number) => {
    setRemaining((current) => current.filter((pair) => pair.remote.id !== remoteId));
  };

  const merge = async (pair: MergeCandidate) => {
    setBusyId(pair.remote.id);
    setErrors((current) => ({ ...current, [pair.remote.id]: "" }));
    try {
      const profiles = await mergeProfiles(pair.local.id, pair.remote.id);
      onMerged(profiles);
      dismiss(pair.remote.id);
    } catch (failure) {
      setErrors((current) => ({ ...current, [pair.remote.id]: errorMessage(failure) }));
    } finally {
      setBusyId(null);
    }
  };

  return (
    <Modal open={remaining.length > 0} onClose={onClose} title={t("profiles.merge.title")} className="merge-profiles-dialog">
      <p className="muted-note">{t("profiles.merge.intro")}</p>
      {remaining.map((pair) => (
        <div className="merge-profiles-row" key={pair.remote.id}>
          <p>{t("profiles.merge.pair", { local: pair.local.name, remote: pair.remote.name })}</p>
          {errors[pair.remote.id] && (
            <div className="dialog-error" role="alert">{errors[pair.remote.id]}</div>
          )}
          <div className="settings-actions">
            <button
              className="primary-button"
              disabled={busyId === pair.remote.id}
              onClick={() => void merge(pair)}
            >
              {busyId === pair.remote.id ? t("profiles.merge.merging") : t("profiles.merge.mergeButton")}
            </button>
            <button
              className="secondary-button"
              disabled={busyId === pair.remote.id}
              onClick={() => dismiss(pair.remote.id)}
            >
              {t("profiles.merge.notSamePerson")}
            </button>
          </div>
        </div>
      ))}
      <div className="settings-actions">
        <button className="secondary-button" onClick={onClose}>{t("profiles.merge.done")}</button>
      </div>
    </Modal>
  );
}
