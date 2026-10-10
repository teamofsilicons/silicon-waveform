"use client";
import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { AudioLines, Mic, Volume2, Copy, Download } from "lucide-react";
import { Page,PageHeader,Section,Stack,Cluster,Grid,Surface,Button,Input,Textarea,Select,Switch,api,ErrorBox,useVoices,usePreferences,options,type SpeechResult } from "./common";
import { ProviderControls,controlPayload,type Controls } from "./provider-controls";
import styles from "./waveform.module.css";
export function Speech(){
 const cache=useQueryClient();const voices=useVoices();const prefs=usePreferences();
 const [mode,setMode]=useState<"tts"|"stt">("tts");const [text,setText]=useState("");const [file,setFile]=useState("");const [lang,setLang]=useState("");const [voice,setVoice]=useState("default");const [provider,setProvider]=useState("default");const [fallback,setFallback]=useState(false);const [controls,setControls]=useState<Controls>({});const [keys,setKeys]=useState<Record<string,string>>({});const [busy,setBusy]=useState(false);const [error,setError]=useState<unknown>(null);const [result,setResult]=useState<SpeechResult|null>(null);const [copied,setCopied]=useState(false);const [retry,setRetry]=useState<{signature:string;key:string}|null>(null);
 const chain=mode==="tts"?["gemini","elevenlabs","openai"]:["gemini","openai","deepgram"];const resolved=provider==="default"?(mode==="tts"?prefs.data?.tts_order[0]:prefs.data?.stt_order[0])??"gemini":provider;
 async function submit(e:React.FormEvent){
  e.preventDefault();setError(null);setBusy(true);setResult(null);
  const providerKeys=Object.fromEntries(Object.entries(keys).filter(([k,v])=>chain.includes(k)&&v));
  const body={...(mode==="tts"?{text,auto_fallback:fallback,...(voice!=="default"?{voice_profile:voice}:{}),...(!fallback&&Object.keys(controlPayload(controls)).length?{provider_options:{[resolved]:controlPayload(controls)}}:{})}:{file_url:file}),...(lang?(mode==="tts"?{lang}:{language:lang}):{}),...(provider!=="default"?{provider_order:[provider]}:{}),...(Object.keys(providerKeys).length?{provider_keys:providerKeys}:{})};
  const signature=JSON.stringify({mode,...body});const key=retry?.signature===signature?retry.key:crypto.randomUUID();setRetry({signature,key});
  try{const answer=await api.post<SpeechResult>("/api/v1/"+mode,body,{idempotencyKey:key});setResult(answer);setKeys({});setRetry(null);await cache.invalidateQueries({queryKey:["history"]});}catch(e){setError(e);}finally{setBusy(false);}
 }
 function download(){if(!result?.transcript)return;const url=URL.createObjectURL(new Blob([result.transcript],{type:"text/plain"}));const a=document.createElement("a");a.href=url;a.download="waveform-"+result.request_id+".txt";a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}
 return <Page width="reading">
  <PageHeader title="Your words, in another form" description="Create a voice recording or turn an audio file into a transcript."/>
  <Stack gap={8}>
   <Cluster><Button variant={mode==="tts"?"primary":"secondary"} onClick={()=>{setMode("tts");setProvider("default");setResult(null);setControls({});}}><Volume2 size={16}/>Text to speech</Button><Button variant={mode==="stt"?"primary":"secondary"} onClick={()=>{setMode("stt");setProvider("default");setResult(null);setControls({});}}><Mic size={16}/>Speech to text</Button></Cluster>
   <form onSubmit={submit}><Stack gap={6}>
    {mode==="tts"?<Textarea label="What should the voice say?" value={text} onChange={e=>setText(e.target.value)} rows={7} required description={Array.from(text).length.toLocaleString()+" / 4,096 characters"} placeholder="Put your ideas into words…"/>:<Input label="Briefcase audio URL" type="url" required value={file} onChange={e=>setFile(e.target.value)} description="Use a permanent audio-file URL from Briefcase that your account can read." placeholder="https://briefcase.teamofsilicons.com/…"/>}
    <Grid><Select label="Provider" value={provider} onValueChange={v=>{setProvider(v);setControls({});}} options={[{value:"default",label:"Saved order · "+resolved},...options(chain)]}/>{mode==="tts"&&<Select label="Voice profile" value={voice} onValueChange={setVoice} options={[{value:"default",label:"My default voice"},...(voices.data?.items??[]).map(v=>({value:v.id,label:v.name+(v.auditioned?"":" · provisional")}))]}/>}<Input label="Language hint" value={lang} onChange={e=>setLang(e.target.value)} placeholder="Auto detect, or en-US"/></Grid>
    <ErrorBox error={voices.error||prefs.error}/>
    {mode==="tts"?<Switch label="Try another provider if the first cannot complete" checked={fallback} onCheckedChange={v=>{setFallback(v);if(v)setControls({});}}/>:<p className={styles.muted}>Transcription automatically tries the remaining providers in your saved order if needed.</p>}
    {mode==="tts"&&!fallback&&<details className={styles.details}><summary>Voice direction & provider controls</summary><ProviderControls provider={resolved} value={controls} onChange={setControls}/></details>}
    <details className={styles.details}><summary>Use a provider key for this request</summary><Grid>{chain.map(p=><Input key={p} label={options([p])[0].label+" API key"} type="password" autoComplete="off" value={keys[p]??""} onChange={e=>setKeys({...keys,[p]:e.target.value})}/>)}</Grid><p className={styles.muted}>Request keys take priority over saved keys and are cleared after success.</p></details>
    <ErrorBox error={error}/><Cluster><Button type="submit" loading={busy} disabled={busy||(mode==="tts"&&Array.from(text).length>4096)}><AudioLines size={16}/>{mode==="tts"?"Create speech":"Transcribe audio"}</Button>{busy&&<span role="status" className={styles.muted}>Working with the provider…</span>}</Cluster>
   </Stack></form>
   {result&&<Section title={result.transcript!==undefined?"Your transcript":"Your recording"} description={result.provider+(result.duration_ms!=null?" · "+(result.duration_ms/1000).toFixed(1)+" seconds":"")}><Surface><Stack>
    {result.transcript!==undefined?<><p className={styles.transcript}>{result.transcript}</p><Cluster><Button variant="secondary" onClick={async()=>{await navigator.clipboard.writeText(result.transcript??"");setCopied(true);}}><Copy size={16}/>{copied?"Copied":"Copy transcript"}</Button><Button variant="secondary" onClick={download}><Download size={16}/>Download text</Button></Cluster>{result.detected_language&&<p>Detected language: {result.detected_language}</p>}</>:<>
     <audio controls preload="none" src={result.audio_url?"/api/api/v1/audio/"+encodeURIComponent(result.request_id):result.file_url??undefined} className={styles.audio}/>
     {result.file_url&&<a href={result.file_url} target="_blank" rel="noreferrer">Open recording in Briefcase</a>}
     {result.audio_url&&<a href={"/api/api/v1/audio/"+encodeURIComponent(result.request_id)} download>Download MP3</a>}
     <p>{result.storage?.message??(result.storage?.status==="stored"?"Saved in Briefcase.":"This recording is being held temporarily.")}</p>{result.storage?.expires_at&&<p className={styles.muted}>Available until {new Date(result.storage.expires_at).toLocaleString()}.</p>}
    </>}
    <small className={styles.muted}>Request {result.request_id}</small>
   </Stack></Surface></Section>}
  </Stack>
 </Page>;
}
