/**
 * sparkdown/full — CommonMark + every extension → HTML, as WASI-free WebAssembly.
 *
 * @example
 * import { toHtml } from "@momiji-rs/sparkdown/full";
 * const html = await toHtml(":::note\nhi `:tada:` ~~x~~\n:::");
 * // toggle a flag:
 * const a = await toHtml(md, { externalLinks: true, frontmatter: false });
 */

/**
 * Extension flags. GFM + the content extensions default to `true`; the opinionated
 * transforms (`hardWraps`, `headingIds`, `externalLinks`) default to `false`.
 */
export interface Options {
  // GFM
  strikethrough?: boolean;
  tasklist?: boolean;
  autolink?: boolean;
  tagfilter?: boolean;
  tables?: boolean;
  footnotes?: boolean;
  // content extensions
  emoji?: boolean;
  diagram?: boolean;
  frontmatter?: boolean;
  deflist?: boolean;
  directives?: boolean;
  // transforms (default off)
  hardWraps?: boolean;
  headingIds?: boolean;
  externalLinks?: boolean;
}

/**
 * Synchronously instantiate the wasm module (idempotent); afterwards
 * {@link toHtmlSync} works with no await. For Node/Bun/Deno/server/workers — NOT
 * the browser main thread (synchronous compile is capped at ~4 KB there; use
 * {@link init}/{@link ready} in browsers). Returns the raw wasm exports.
 */
export function initSync(): unknown;

/** Instantiate the wasm module asynchronously (idempotent). Resolves once it is ready. */
export function init(): Promise<unknown>;

/** Resolves once the wasm module is ready; afterwards {@link toHtmlSync} works (lazy). */
export const ready: PromiseLike<void>;

/** Render `markdown` to HTML with the full extension set (see {@link Options} for defaults). */
export function toHtml(markdown: string, options?: Options): Promise<string>;

/** Synchronous render — valid only after `await ready` (or a prior `toHtml`). */
export function toHtmlSync(markdown: string, options?: Options): string;

declare const _default: typeof toHtml;
export default _default;
