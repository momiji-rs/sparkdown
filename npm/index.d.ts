/**
 * sparkdown — fast, standards-first CommonMark → HTML, as WASI-free WebAssembly.
 *
 * @example
 * import { toHtml } from "@momiji-rs/sparkdown";
 * const html = await toHtml("# Hello *world*");
 */

/** Instantiate the wasm module (idempotent). Resolves once it is ready. */
export function init(): Promise<unknown>;

/** Resolves once the wasm module is ready; afterwards {@link toHtmlSync} works. */
export const ready: Promise<void>;

/** Render CommonMark `markdown` to an HTML string. */
export function toHtml(markdown: string): Promise<string>;

/** Synchronous render — valid only after `await ready` (or a prior `toHtml`). */
export function toHtmlSync(markdown: string): string;

declare const _default: typeof toHtml;
export default _default;
