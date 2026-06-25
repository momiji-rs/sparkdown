/**
 * sparkdown-gfm — CommonMark + GitHub Flavored Markdown → HTML, as WASI-free
 * WebAssembly.
 *
 * @example
 * import { toHtml } from "@momiji-rs/sparkdown-gfm";
 * const html = await toHtml("~~done~~ and www.example.com");
 * // disable an extension:
 * const plain = await toHtml(md, { autolink: false });
 */

/** GFM extension flags. Omitted flags default to `true` (hardWraps to `false`). */
export interface Options {
  strikethrough?: boolean;
  tasklist?: boolean;
  autolink?: boolean;
  tagfilter?: boolean;
  tables?: boolean;
  hardWraps?: boolean;
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

/** Render `markdown` to HTML with GFM extensions (default: all on). */
export function toHtml(markdown: string, options?: Options): Promise<string>;

/** Synchronous render — valid only after `await ready` (or a prior `toHtml`). */
export function toHtmlSync(markdown: string, options?: Options): string;

declare const _default: typeof toHtml;
export default _default;
