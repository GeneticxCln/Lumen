import { useEffect, useRef } from "react";
import type { ReactNode } from "react";
import { Icon } from "./Icon";
import { t } from "../i18n/index.ts";

export function Modal({ open, onClose, title, children, className = "", keepMounted = false }: { open: boolean; onClose: () => void; title: string; children: ReactNode; className?: string; keepMounted?: boolean }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  }, [open]);
  return <dialog ref={ref} className={`source-dialog ${className}`} onCancel={(event) => { event.preventDefault(); onClose(); }} onClick={(event) => { if (event.target === ref.current) { const bounds = ref.current.getBoundingClientRect(); if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) onClose(); } }} aria-label={title}>
    <div className="dialog-header"><div><span className="eyebrow">{t("modal.eyebrow")}</span><h2>{title}</h2></div><button className="icon-button" aria-label={t("modal.close", { title })} onClick={onClose}><Icon name="close" /></button></div>
    {(open || keepMounted) && children}
  </dialog>;
}
