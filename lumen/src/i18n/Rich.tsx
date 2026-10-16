import { Fragment } from "react";
import type { ReactNode } from "react";
import { tRich } from "./index.ts";
import type { PlainKey } from "./index.ts";

type Tags = Record<string, (text: string) => ReactNode>;

/**
 * A message with `<tag>text</tag>` markup, with each tag rendered by the matching function in
 * `tags`. Only for messages without `{placeholders}`; use `tRich` directly for the rest.
 */
export function Rich({ id, tags }: { id: PlainKey; tags: Tags }) {
  return (
    <>
      {tRich(id).map((part, index) => (
        <Fragment key={index}>{part.tag && tags[part.tag] ? tags[part.tag](part.text) : part.text}</Fragment>
      ))}
    </>
  );
}
