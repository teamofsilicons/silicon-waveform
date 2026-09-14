import {readFile,readdir,stat} from 'node:fs/promises';
import {join,resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
const root=join(dirname(fileURLToPath(import.meta.url)),'dist'),pages=[];
async function walk(dir){for(const e of await readdir(dir,{withFileTypes:true})){const p=join(dir,e.name);if(e.isDirectory())await walk(p);else if(e.name==='index.html')pages.push(p);}}
await walk(root);let failures=[];
for(const page of pages){const html=await readFile(page,'utf8');if(!html.includes('rel="canonical" href="https://docs.waveform.teamofsilicons.com/'))failures.push(`${page}: canonical`);
for(const [,href] of html.matchAll(/href="(\/[^"#]*)(?:#[^"]*)?"/g)){const target=resolve(root,'.'+decodeURI(href));if(!target.startsWith(root))throw Error('Escaping link');try{await stat(href.endsWith('/')?join(target,'index.html'):target)}catch{failures.push(`${page}: ${href}`)}}
for(const [,id] of html.matchAll(/href="#([^"]+)"/g))if(!html.includes(`id="${id}"`))failures.push(`${page}: #${id}`);
}
if(failures.length)throw Error(failures.join('\n'));console.log(`Verified ${pages.length} pages, local links, navigation anchors and canonical URLs.`);
