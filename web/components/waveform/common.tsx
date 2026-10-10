"use client";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/client/api";
import { Select } from "@/components/silicon-ui/select/select";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
export { Button } from "@/components/silicon-ui/button/button";
export { Input } from "@/components/silicon-ui/input/input";
export { Textarea } from "@/components/silicon-ui/textarea/textarea";
export { Select } from "@/components/silicon-ui/select/select";
export { Switch } from "@/components/silicon-ui/switch/switch";
export { Page, PageHeader, Section, Stack, Cluster, Grid, Surface, SettingsGroup, SettingsRow } from "@/components/foundation/layout/layout";
export { api };
export type Account = {uuid:string;id:string;kind:"carbon"|"silicon";display_name?:string;pfp_url?:string;custodian?:{uuid:string;id:string}};
export type Voice={id:string;name:string;description:string;revision:number;auditioned:boolean};
export type Preferences={voice_profile:string|null;telemetry_enabled:boolean;tts_order:string[];stt_order:string[];defaults:{voice_profile:string|null;tts_order:string[];stt_order:string[]}};
export type Job={id:string;operation:"tts"|"stt";status:string;first_line:string;duration_ms:number|null;provider:string|null;error_code:string|null;created_at:string;finished_at:string|null;issuing_app?:string|null;voice_profile?:{id:string;revision:number}};
export type SpeechResult={request_id:string;provider:string;duration_ms:number|null;file_url?:string|null;audio_url?:string|null;transcript?:string;detected_language?:string|null;storage?:{status:string;message?:string;expires_at?:string}};
export const providers = {gemini:"Gemini",openai:"OpenAI",elevenlabs:"ElevenLabs",deepgram:"Deepgram"};
export const options=(items:string[])=>items.map(value=>({value,label:providers[value as keyof typeof providers]??value}));
export const accountQuery=(account:string)=>account==="me"?{}:{account};
export function ErrorBox({error}:{error:unknown}) {if(!error)return null;const e=error as {message?:string;hint?:string;requestId?:string;status?:number};return <ErrorAlert error={{message:e.message??"Something went wrong.",hint:e.hint,requestId:e.requestId,status:e.status??0}}/>;}
export function useVoices(){return useQuery({queryKey:["voices"],queryFn:()=>api.get<{items:Voice[]}>("/api/v1/voice-profiles")});}
export function usePreferences(account="me"){return useQuery({queryKey:["preferences",account],queryFn:()=>api.get<Preferences>("/api/v1/preferences",{query:accountQuery(account)})});}
export function AccountPicker({value,onChange}:{value:string;onChange:(value:string)=>void}) {const q=useQuery({queryKey:["silicons"],queryFn:()=>api.get<{items:Account[]}>("/api/v1/silicons")});return <><Select label="Account" value={value} onValueChange={onChange} options={[{value:"me",label:"My account"},...(q.data?.items??[]).map(a=>({value:a.uuid,label:(a.display_name||a.id)+" · "+a.id}))]}/><ErrorBox error={q.error}/></>;}
export function date(value:string){return new Date(value).toLocaleString();}
