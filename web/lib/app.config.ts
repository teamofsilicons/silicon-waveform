import { AudioLines, History, Settings2, Mic, Volume2, KeyRound, Bot, TerminalSquare, FolderOpen } from "lucide-react";
import type { AppConfig } from "./app-config.types";
export const appConfig: AppConfig = {
  appId: "waveform", name: "Waveform", brandPrefix: "Silicon", tagline: "Give words a voice. Give audio a transcript.",
  description: "Turn text into speech and audio into text with Gemini, OpenAI, ElevenLabs and Deepgram. Keep your voices, provider keys and speech history together, with files saved in Briefcase.",
  mark: { paths: ["M2 10v4", "M6 6v12", "M10 3v18", "M14 8v8", "M18 5v14", "M22 10v4"] },
  cli: { command: "waveform" }, home: "/speech",
  nav: [{href:"/speech",label:"Speech",icon:AudioLines},{href:"/history",label:"History",icon:History},{href:"/settings",label:"Settings",icon:Settings2}],
  links:{docs:"https://docs.waveform.teamofsilicons.com",store:"https://apps.teamofsilicons.com/apps/waveform",source:"https://github.com/teamofsilicons/silicon-waveform"},
  signIn:{scopes:["email"]}, api:{forwardHeaders:["x-waveform-contract"],exposeHeaders:["x-waveform-contract","retry-after"]},
  landing:{headline:"Make yourself heard",lede:"A familiar place to create speech, transcribe audio and find your words again. Choose the right voice and provider for every idea.",forCarbons:[{icon:Volume2,title:"Words, spoken naturally",text:"Choose a voice profile or tune a provider's own voice and direction controls."},{icon:Mic,title:"Audio, made readable",text:"Transcribe an audio file from Briefcase, with automatic fallback when a provider is unavailable."},{icon:KeyRound,title:"Bring your own keys",text:"Save your provider credentials or use a key just once for a request."},{icon:FolderOpen,title:"Keep your recordings",text:"Approve Briefcase access to save generated audio with your files."}],forSilicons:[{icon:TerminalSquare,title:"Ready at the command line",text:"Install with Silicon Apps and use the same speech API from your own tools."},{icon:Bot,title:"A voice for your work",text:"Sign in with Silicon Accounts. Your custodian can manage your preferences and review your history."}]} };
export type { AppConfig, NavItem } from "./app-config.types";
