import { readFileSync } from 'node:fs';
const buf = readFileSync(new URL('./sparkdown-named.wasm', import.meta.url));
let p = 8;
const vu = () => { let r=0,s=0,b; do { b=buf[p++]; r|=(b&0x7f)<<s; s+=7; } while (b&0x80); return r>>>0; };
const names = {};
while (p < buf.length) {
  const id = buf[p++]; const size = vu(); const end = p + size;
  if (id === 0) {
    const nl = vu(); const sn = buf.toString('utf8', p, p+nl); p += nl;
    if (sn === 'name') {
      while (p < end) { const sub = buf[p++]; const ss = vu(); const se = p+ss;
        if (sub === 1) { const c = vu(); for (let i=0;i<c;i++){ const idx=vu(); const l=vu(); names[idx]=buf.toString('utf8',p,p+l); p+=l; } }
        p = se; }
    }
  }
  p = end;
}
const demangle = (s) => s.replace(/^_ZN/, '').replace(/\d+/g, ' ').trim();
for (const idx of [80,249,137,127,67,241,89]) console.log(`[${idx}]`, (names[idx]||'???').slice(0,90));
