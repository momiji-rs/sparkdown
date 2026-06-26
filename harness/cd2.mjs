// Gate 2 per-node-type diff lister: prints every example whose first position
// diff (vs mdast-util-from-markdown) is on a node of the given type, with the
// failing field, our/ref values, and the source. Usage: `node cd2.mjs <type>`
// (default "code"). Run from harness/ after regenerating sparkdown-mdast.json.
import { readFileSync } from 'node:fs';
import { fromMarkdown } from 'mdast-util-from-markdown';
const data = JSON.parse(readFileSync(new URL('./sparkdown-mdast.json', import.meta.url)));
const INLINE = new Set(['text','emphasis','strong','delete','inlineCode','break','link','image','linkReference','imageReference','html']);
function firstPosDiff(a, b) {
  if (!a || !b || typeof a !== 'object') return null;
  const pa = a.position, pb = b.position;
  if (pb) for (const end of ['start','end']) for (const f of ['line','column','offset']) {
    const va = pa?.[end]?.[f], vb = pb?.[end]?.[f];
    if (va !== vb) return { type: a.type, field: `${end}.${f}`, ours: va, ref: vb };
  }
  const ca=a.children||[], cb=b.children||[];
  for (let i=0;i<Math.max(ca.length,cb.length);i++){const d=firstPosDiff(ca[i],cb[i]); if(d) return d;}
  return null;
}
const want = process.argv[2]||'code';
data.forEach((e,i)=>{
  const ref = fromMarkdown(e.markdown);
  const d = firstPosDiff(e.mdast, ref);
  if (d && d.type===want){
    console.log(`#${i} ${d.field} ours=${d.ours} ref=${d.ref}  md=${JSON.stringify(e.markdown).slice(0,60)}`);
  }
});
