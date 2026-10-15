// Decides whether a search result passes the "hide other languages" filter.
// Library items (they have a channel_id) pass when their playlist's tvg-language is the chosen
// one or is missing; a library item with no label is not known to be in another language.
// External results pass only when their language matches the chosen one; an unlabeled
// external result is not known to be in that language, so it is hidden.
export function matchesLanguage(result: { language?: string | null; channel_id?: number | null }, chosen: string): boolean {
  if (!chosen) return true;
  if (result.channel_id !== undefined && result.channel_id !== null) {
    return !result.language || result.language === chosen;
  }
  return result.language === chosen;
}
