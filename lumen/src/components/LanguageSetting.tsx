import { useState } from "react";
import { availableLocales } from "../i18n/index.ts";
import { chooseLocale, chosenLocale } from "../i18n/locale.ts";
import { t } from "../i18n/index.ts";

/** The language of the interface in its own name. */
function ownName(code: string): string {
  try {
    return new Intl.DisplayNames([code], { type: "language" }).of(code) ?? code;
  } catch {
    return code;
  }
}

/**
 * Lets the user choose the interface language, or follow the system. Not shown while only one
 * language exists, since there would be nothing to choose.
 */
export function LanguageSetting() {
  const codes = availableLocales();
  const [chosen, setChosen] = useState(chosenLocale() ?? "");
  if (codes.length < 2) return null;

  return (
    <div className="setting-row">
      <div>
        <label htmlFor="setting-language">{t("language.label")}</label>
        <p>{t("language.description")}</p>
      </div>
      <select
        id="setting-language"
        value={chosen}
        onChange={(event) => {
          setChosen(event.target.value);
          chooseLocale(event.target.value || null);
          window.location.reload();
        }}
      >
        <option value="">{t("language.system")}</option>
        {codes.map((code) => <option key={code} value={code}>{ownName(code)}</option>)}
      </select>
    </div>
  );
}
