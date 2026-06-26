import { readFileSync } from 'node:fs';
import { fromMarkdown } from 'mdast-util-from-markdown';
const data = JSON.parse(readFileSync(new URL('./sparkdown-mdast.json', import.meta.url)));

// find first position diff between our tree and ref, return {type, field, ours, ref}
function firstPosDiff(a, b, path='$') {
  if (!a || !b || typeof a !== 'object') return null;
  // compare position of this node
  const pa = a.position, pb = b.position;
  if (pb) {
    for (const end of ['start','end']) {
      for (const f of ['line','column','offset']) {
        const va = pa?.[end]?.[f], vb = pb?.[end]?.[f];
        if (va !== vb) return { type: a.type, field: `${end}.${f}`, ours: va, ref: vb, isInline: INLINE.has(a.type) };
      }
    }
  }
  const ca = a.children||[], cb = b.children||[];
  for (let i=0;i<Math.max(ca.length,cb.length);i++){
    const d = firstPosDiff(ca[i], cb[i], `${path}.${i}`);
    if (d) return d;
  }
  return null;
}
const INLINE = new Set(['text','emphasis','strong','delete','inlineCode','break','link','image','linkReference','imageReference','html']);

let pass=0; const byType={}, byField={}, blockVsInline={block:0,inline:0};
for (const e of data) {
  const ref = fromMarkdown(e.markdown);
  const d = firstPosDiff(e.mdast, ref);
  if (!d) { pass++; continue; }
  byType[d.type]=(byType[d.type]||0)+1;
  byField[d.field]=(byField[d.field]||0)+1;
  blockVsInline[d.isInline?'inline':'block']++;
}
console.log(`Gate2 position pass: ${pass}/${data.length} (${(100*pass/data.length).toFixed(1)}%)`);
console.log(`\nfirst-diff is on a BLOCK node: ${blockVsInline.block},  INLINE node: ${blockVsInline.inline}`);
console.log(`\nfirst-diff by node type:`); for(const[k,v]of Object.entries(byType).sort((a,b)=>b[1]-a[1])) console.log(`  ${k.padEnd(16)} ${v}`);
console.log(`\nfirst-diff by field:`); for(const[k,v]of Object.entries(byField).sort((a,b)=>b[1]-a[1])) console.log(`  ${k.padEnd(16)} ${v}`);
