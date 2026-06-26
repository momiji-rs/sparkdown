// Type definitions for @momiji-rs/sparkdown/mdast

/** Opt-in grammar extensions (all off by default = pure CommonMark). */
export interface Options {
  // GFM
  strikethrough?: boolean;
  tasklist?: boolean;
  autolink?: boolean;
  tagfilter?: boolean;
  tables?: boolean;
  footnotes?: boolean;
  // content extensions
  frontmatter?: boolean;
  emoji?: boolean;
  diagram?: boolean;
  deflist?: boolean;
  directives?: boolean;
  // transforms
  hardWraps?: boolean;
  headingIds?: boolean;
  externalLinks?: boolean;
}

/**
 * A unist/mdast node. Loosely typed so the package needs no hard `@types/mdast`
 * dependency; assignable to/from the `mdast` `Root`/`Nodes` types when present.
 */
export interface MdastNode {
  type: string;
  children?: MdastNode[];
  value?: string;
  position?: unknown;
  [key: string]: unknown;
}
export type MdastRoot = MdastNode;

/** Synchronously instantiate the wasm (Node/Bun/Deno/edge; not the browser main thread). */
export function initSync(): unknown;
/** Asynchronously instantiate the wasm (idempotent). */
export function init(): Promise<unknown>;
/** Resolves once the wasm is ready; then the `*Sync` forms and the parser work. */
export const ready: PromiseLike<void>;

/** Parse markdown → an mdast (unist) tree. */
export function toMdast(markdown: string, options?: Options): Promise<MdastRoot>;
/** Synchronous parse — valid only after `await ready` / `initSync()`. */
export function toMdastSync(markdown: string, options?: Options): MdastRoot;

/**
 * Render markdown → HTML through the in-wasm mdast → HTML pass — byte-identical to
 * `mdast-util-to-hast` + `hast-util-to-html`.
 */
export function toHtml(markdown: string, options?: Options): Promise<string>;
/** Synchronous render — valid only after `await ready` / `initSync()`. */
export function toHtmlSync(markdown: string, options?: Options): string;

/**
 * unified plugin: install sparkdown as the processor's parser (drop-in for
 * `remark-parse`). In the browser, `await sparkdownParse.ready` before
 * `.processSync(...)`.
 */
declare function sparkdownParse(this: unknown, options?: Options): void;
declare namespace sparkdownParse {
  const ready: PromiseLike<void>;
}
export default sparkdownParse;
