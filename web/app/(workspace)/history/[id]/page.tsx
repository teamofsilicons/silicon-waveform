import { JobDetail } from "@/components/waveform/history";
export const metadata={title:"Speech details"};
export default async function DetailPage({params,searchParams}:{params:Promise<{id:string}>;searchParams:Promise<{account?:string}>}){return <JobDetail id={(await params).id} account={(await searchParams).account??"me"}/>;}
