# @momiji-rs/sparkdown-gfm

CommonMark **+ GitHub Flavored Markdown** → HTML, as a **WASI-free** WebAssembly
build. Same engine as [`@momiji-rs/sparkdown`](https://www.npmjs.com/package/@momiji-rs/sparkdown),
compiled with the GFM extensions. Zero dependencies, self-contained (the wasm is
base64-inlined); runs in Node, browsers, bundlers, Deno, Bun, and edge runtimes.

```bash
npm install @momiji-rs/sparkdown-gfm
```

```js
import { toHtml } from "@momiji-rs/sparkdown-gfm";

await toHtml("~~done~~ and www.example.com");
// disable an extension:
await toHtml(md, { autolink: false });
```

Extensions (all default **on**, `hardWraps` off): `strikethrough`, `tasklist`,
`autolink`, `tagfilter`, `tables`, `hardWraps`. For pure CommonMark with a
smaller module, use `@momiji-rs/sparkdown`.

See the [main project README](https://github.com/momiji-rs/sparkdown#readme).

## License

MIT.
