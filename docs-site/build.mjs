import {readFile,writeFile,mkdir,rm,cp,readdir} from 'node:fs/promises';
import {dirname,join,resolve,relative,posix} from 'node:path';
import {fileURLToPath} from 'node:url';
import MarkdownIt from 'markdown-it';
const here=dirname(fileURLToPath(import.meta.url)), root=resolve(here,'..'), out=join(here,'dist');
const origin='https://docs.waveform.teamofsilicons.com';
const md=new MarkdownIt({html:false,linkify:true});
const esc=s=>String(s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const slug=s=>s.toLowerCase().replace(/[^\p{L}\p{N}\s_-]/gu,'').trim().replace(/\s+/g,'-');
const pages=[];
async function collect(dir){for(const entry of await readdir(dir,{withFileTypes:true})){const file=join(dir,entry.name);if(entry.isDirectory()) await collect(file);else if(entry.name.endsWith('.md')){const source=relative(join(root,'docs'),file);const text=await readFile(file,'utf8');const route=source==='README.md'?'/':`/${source.replace(/README\.md$/,'').replace(/\.md$/,'').replace(/\/$/,'')}/`;pages.push({source,text,route,title:text.match(/^# (.+)$/m)?.[1]||entry.name});}}}
await collect(join(root,'docs'));
const bySource=new Map(pages.map(p=>[p.source,p.route]));
const groups=[['Start here',['README.md','testing.md']],['Use Waveform',['cli.md','voice-profiles.md','configuration.md']],['Build with Waveform',['client.md','api.md','iam.md','development.md','api-contracts.md','honeycomb-lifecycle.md','releases.md']]];
await rm(out,{recursive:true,force:true});await mkdir(out,{recursive:true});
await cp(join(here,'style.css'),join(out,'style.css'));
await cp(join(root,'openapi.yaml'),join(out,'openapi.yaml'));
await cp(join(root,'honeycomb.yaml'),join(out,'honeycomb.yaml'));
await cp(join(root,'scripts/install.sh'),join(out,'install.sh'));
const config=JSON.parse(await readFile(join(here,'vercel.json'),'utf8')); config.buildCommand=''; delete config.outputDirectory; await writeFile(join(out,'vercel.json'),JSON.stringify(config,null,2));
for(const page of pages){
 const tokens=md.parse(page.text,{}),toc=[],counts=new Map();
 for(let i=0;i<tokens.length;i++){
  const t=tokens[i];if(t.type==='heading_open'){const label=tokens[i+1].content;let id=slug(label);const n=counts.get(id)||0;counts.set(id,n+1);if(n)id+=`-${n}`;t.attrSet('id',id);if(t.tag==='h2')toc.push([id,label]);}
  for(const child of t.children||[])if(child.type==='link_open'){
   const href=child.attrGet('href');if(!href||/^(https?:|mailto:|#)/.test(href))continue;
   const [path,hash]=href.split('#');let source=posix.normalize(posix.join(posix.dirname(page.source),path));
   let target=bySource.get(source)||bySource.get(posix.join(source,'README.md'));
   if(source==='../openapi.yaml')target='/openapi.yaml';
   if(!target)target=`https://github.com/teamofsilicons/silicon-waveform/blob/main/${source.startsWith('../')?source.slice(3):'docs/'+source}`;
   child.attrSet('href',target+(hash?'#'+hash:''));
  }
 }
 const nav=groups.map(([title,sources])=>`<div class="nav-group"><span>${title}</span>${sources.map(source=>{const p=pages.find(p=>p.source===source);if(!p)throw Error(source);return `<a href="${p.route}"${p.route===page.route?' aria-current="page"':''}>${esc(p.title.replace(/^Silicon Waveform[: —-]?\s*/,'')||'Overview')}</a>`}).join('')}</div>`).join('');
 const body=md.renderer.render(tokens,md.options,{});
 const html=`<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${esc(page.title)} · Waveform Docs</title><meta name="description" content="Install Silicon Waveform, generate speech and transcribe audio, and build integrations with the CLI, Rust client, and API."><link rel="canonical" href="${origin}${page.route}"><link rel="stylesheet" href="/style.css"></head><body><a class="skip" href="#content">Skip to content</a><header><a class="brand" href="/">◈ <b>Silicon Waveform</b> <span>Docs</span></a><nav><a href="/">Quickstart</a><a href="/openapi.yaml">OpenAPI ↗</a><a href="https://github.com/teamofsilicons/silicon-waveform">GitHub ↗</a></nav></header><div class="layout"><aside class="sidebar"><p class="version">Waveform · DOCUMENTATION</p>${nav}</aside><main id="content"><div class="crumb">Waveform / ${esc(page.source==='README.md'?'Overview':page.title)}</div><article>${body}</article><footer>Team of Silicons · <a href="https://github.com/teamofsilicons/silicon-waveform/blob/main/docs/${page.source}">Edit this page</a><p>Usage guides first. Protocol details when you need them.</p></footer></main><aside class="toc"><span>ON THIS PAGE</span>${toc.map(([id,title])=>`<a href="#${esc(id)}">${esc(title)}</a>`).join('')}</aside></div></body></html>`;
 const destination=join(out,page.route,'index.html');await mkdir(dirname(destination),{recursive:true});await writeFile(destination,html);
}
await writeFile(join(out,'llms.txt'),'# Silicon Waveform\n\n'+pages.map(p=>`- [${p.title}](${origin}${p.route}): ${p.source}`).join('\n'));
await writeFile(join(out,'llms-full.txt'),pages.map(p=>`${p.text}\n\nSource: ${origin}${p.route}`).join('\n\n---\n\n'));
await writeFile(join(out,'sitemap.xml'),`<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${pages.map(p=>`<url><loc>${origin}${p.route}</loc></url>`).join('')}</urlset>`);
await writeFile(join(out,'robots.txt'),`User-agent: *\nAllow: /\nSitemap: ${origin}/sitemap.xml\n`);
await writeFile(join(out,'404.html'),'<!doctype html><html lang="en"><meta charset="utf-8"><title>Page not found · Waveform Docs</title><link rel="stylesheet" href="/style.css"><main><h1>Page not found</h1><p><a href="/">Return to Silicon Waveform documentation.</a></p></main></html>');
console.log(`Built ${pages.length} documentation pages.`);
