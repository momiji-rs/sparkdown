/**
 * sparkdown — fast, standards-first CommonMark → HTML, as WASI-free WebAssembly.
 *
 * @example
 * import { toHtml } from "@momiji-rs/sparkdown";
 * const html = await toHtml("# Hello *world*");
 */

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

/** Render CommonMark `markdown` to an HTML string. */
export function toHtml(markdown: string): Promise<string>;

/** Synchronous render — valid only after `await ready` (or a prior `toHtml`). */
export function toHtmlSync(markdown: string): string;

declare const _default: typeof toHtml;
export default _default;
