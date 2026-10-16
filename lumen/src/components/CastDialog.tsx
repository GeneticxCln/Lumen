import { useCallback, useEffect, useState } from "react";
import { castDiscover, castStart, errorMessage } from "../lib/tauri";
import type { CastDevice, CastState, PlayingItem } from "../lib/types";
import { t } from "../i18n/index.ts";
import { Modal } from "./Modal";

interface BodyProps {
  item: PlayingItem;
  /** Seconds into the title to start at. */
  startAt: number;
  onStarted: (state: CastState) => void;
}

function CastDialogBody({ item, startAt, onStarted }: BodyProps) {
  const [devices, setDevices] = useState<CastDevice[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [sending, setSending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const isWeb = /^https?:\/\//i.test(item.source);

  const search = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setDevices(await castDiscover());
    } catch (failure) {
      setDevices(null);
      setError(errorMessage(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => { void search(); }, [search]);

  const send = async (device: CastDevice) => {
    setSending(device.id);
    setError(null);
    try {
      onStarted(await castStart(device.id, item, startAt));
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setSending(null);
    }
  };

  return (
    <div className="dialog-content cast-dialog">
      <p>{t("cast.intro")}</p>
      <p className="muted-note">{isWeb ? t("cast.remoteNote") : t("cast.localNote")}</p>
      <div className="settings-actions">
        <button className="secondary-button" disabled={busy} onClick={() => void search()}>
          {busy ? t("cast.searching") : devices ? t("cast.searchAgain") : t("cast.search")}
        </button>
      </div>
      {error && <div className="dialog-error" role="alert">{error}</div>}
      {devices && devices.length === 0 && !busy && <p className="muted-note">{t("cast.none")}</p>}
      {devices && devices.length > 0 && (
        <ul className="subtitle-results">
          {devices.map((device) => (
            <li key={device.id}>
              <div className="subtitle-row-text"><strong>{t("cast.deviceRow", { name: device.name, address: device.address })}</strong></div>
              <button className="primary-button" disabled={sending !== null} onClick={() => void send(device)}>
                {sending === device.id ? t("cast.sending") : t("cast.send")}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

interface CastDialogProps extends Omit<BodyProps, "item"> {
  open: boolean;
  item: PlayingItem | null;
  onClose: () => void;
}

/** Lists the devices on the network and sends what is playing to the one the user picks. */
export function CastDialog({ open, item, startAt, onStarted, onClose }: CastDialogProps) {
  return (
    <Modal open={open} onClose={onClose} title={t("cast.title")} className="subtitle-dialog">
      {item && <CastDialogBody item={item} startAt={startAt} onStarted={onStarted} />}
    </Modal>
  );
}
