import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
export type Status='Idle'|'Loading'|'Paused'|'Playing'|'Buffering'|'Ended'|'Failed';
export type Command={type:'SetPaused';value:boolean}|{type:'SeekToMs';value:string}|{type:'SetSpeed';value:number}|{type:'SetVolume';value:number}|{type:'SetMuted';value:boolean}|{type:'SelectAudio';value:string}|{type:'SelectSubtitle';value:string|null}|{type:'Stop'};
export interface PlayerView { generation:string; basename:string|null; local:true; status:Status; position_ms:string; duration_ms:string|null; speed:number; volume:number; muted:boolean; tracks:{id:string;kind:'audio'|'subtitle';title:string|null;language:string|null;selected:boolean}[]; decoder:null|{codec:string;hwdec:string|null;video_output:string}; unavailable:boolean }
export const initialView:PlayerView={generation:'',basename:null,local:true,status:'Idle',position_ms:'0',duration_ms:null,speed:1,volume:100,muted:false,tracks:[],decoder:null,unavailable:false};
export function acceptEvent(current:PlayerView,event:PlayerView):PlayerView { return current.generation===event.generation ? event : current; }
export const api={
 snapshot:()=>invoke<PlayerView>('player_snapshot'),
 choose:()=>invoke<PlayerView|null>('choose_media'),
 command:(generation:string,command:Command)=>invoke<void>('player_command',{generation,command}),
 fullscreen:(value:boolean)=>invoke<void>('set_fullscreen',{value}),
 listen:(receive:(view:PlayerView)=>void)=>listen<PlayerView>('player-state',e=>receive(e.payload)),
};
export function time(value:string|null):string {
 if(value===null)return '—';
 const seconds=BigInt(value)/1000n, minutes=seconds/60n;
 return `${minutes.toString().padStart(2,'0')}:${(seconds%60n).toString().padStart(2,'0')}`;
}
export async function togglePlayback(view:PlayerView,send:(command:Command)=>Promise<void>) {
 if(view.status==='Ended'){ await send({type:'SeekToMs',value:'0'}); await send({type:'SetPaused',value:false}); }
 else await send({type:'SetPaused',value:view.status==='Playing'||view.status==='Buffering'});
}
