import { useEffect,useRef,useState } from 'react';
import { acceptEvent,api,initialView,time,togglePlayback,type Command,type PlayerView,type WatchAction } from './player';
const statuses:Record<PlayerView['status'],string>={Idle:'Откройте фильм',Loading:'Загрузка',Paused:'Пауза',Playing:'Воспроизведение',Buffering:'Буферизация',Ended:'Ended · Просмотр завершён',Failed:'Не удалось открыть фильм'};
export function PlayerPanel({view,send=async()=>{},fullscreen=()=>{}}:{view:PlayerView;send?:(command:Command)=>Promise<void>;fullscreen?:()=>void}) {
 const duration=view.duration_ms===null?null:BigInt(view.duration_ms),position=BigInt(view.position_ms);
 const seekable=duration!==null&&duration>0n;
 const ratio=seekable?Number((position>duration?duration:position)*10000n/duration):0;
 const disabled=!view.basename||view.unavailable||view.status==='Loading'||view.status==='Failed';
 return <section className="controls" aria-label="Управление воспроизведением">
  <div className="timeline"><output>{time(view.position_ms)}</output><input type="range" min="0" max="10000" step="1" value={ratio} aria-label="Позиция" disabled={!seekable||disabled} onChange={e=>{if(duration!==null)void send({type:'SeekToMs',value:(duration*BigInt(e.currentTarget.value)/10000n).toString()});}}/><output data-testid="duration">{time(view.duration_ms)}</output></div>
  <div className="buttons">
   <button disabled={disabled} onClick={()=>void togglePlayback(view,send)}>{view.status==='Playing'||view.status==='Buffering'?'Пауза':'Воспроизвести'}</button>
   <label>Скорость <select disabled={disabled} value={view.speed} onChange={e=>void send({type:'SetSpeed',value:Number(e.currentTarget.value)})}>{[.5,.75,1,1.25,1.5,2].map(s=><option key={s} value={s}>{s}×</option>)}</select></label>
   <label>Аудио <select disabled={disabled||!view.tracks.some(t=>t.kind==='audio')} value={view.tracks.find(t=>t.kind==='audio'&&t.selected)?.id??''} onChange={e=>void send({type:'SelectAudio',value:e.currentTarget.value})}><option disabled value="">Нет дорожки</option>{view.tracks.filter(t=>t.kind==='audio').map(t=><option value={t.id} key={t.id}>{t.title??t.language??`Дорожка ${t.id}`}</option>)}</select></label>
   <label>Субтитры <select disabled={disabled} value={view.tracks.find(t=>t.kind==='subtitle'&&t.selected)?.id??''} onChange={e=>void send({type:'SelectSubtitle',value:e.currentTarget.value||null})}><option value="">Выключены</option>{view.tracks.filter(t=>t.kind==='subtitle').map(t=><option value={t.id} key={t.id}>{t.title??t.language??`Дорожка ${t.id}`}</option>)}</select></label>
   <label>Громкость <input aria-label="Громкость" type="range" min="0" max="100" value={view.volume} disabled={disabled} onChange={e=>void send({type:'SetVolume',value:Number(e.currentTarget.value)})}/></label>
   <button disabled={disabled} onClick={()=>void send({type:'SetMuted',value:!view.muted})}>{view.muted?'Включить звук':'Без звука'}</button>
   <button onClick={fullscreen}>На весь экран</button>
  </div>
  <p className="file-name">{view.basename}</p>
 </section>;
}
function WatchPanel({enabled,clearError}:{enabled:boolean;clearError:()=>void}) {
 const [status,setStatus]=useState<'host'|'guest'|'idle'|'disconnected'>('idle'),[invite,setInvite]=useState(''),[share,setShare]=useState(''),[message,setMessage]=useState(''),[busy,setBusy]=useState(false);
 useEffect(()=>{void api.watchStatus().then(setStatus).catch(()=>{});},[]);
 async function run(action:()=>Promise<unknown>,success:string,failure='Не удалось подключиться. Проверьте приглашение и выбранный видеофайл.'){clearError();setBusy(true);setMessage('');try{const result=await action();if(typeof result==='string')setShare(result);setStatus(await api.watchStatus());setMessage(success);}catch(error){setMessage(typeof error==='string'?error:failure);}finally{setBusy(false);}}
 async function copy(){try{await navigator.clipboard.writeText(share);setMessage('Приглашение скопировано');}catch{setMessage('Выделите и скопируйте приглашение вручную');}}
 return <section className="watch-room" aria-label="Совместный просмотр">
  <h2>Совместный просмотр</h2>
  {status==='idle'?<>
   <button disabled={!enabled||busy} onClick={()=>void run(()=>api.watchCreate(),'Сервер создан. Скопируйте ссылку и отправьте гостям.','Не удалось открыть сервер автоматически. Проверьте, что на роутере включён UPnP и провайдер выдаёт публичный IP без CGNAT.')}>{busy?'Создаём сервер…':'Создать сервер'}</button>
   <label>Приглашение от ведущего<textarea placeholder="Вставьте ссылку branch-watch://…" value={invite} onChange={e=>setInvite(e.target.value)}/></label>
   <button disabled={!enabled||busy||!invite} onClick={()=>void run(()=>api.watchJoin(invite),'Вы подключены')}>Подключиться как гость</button>
   <p className="room-help">Branch сама запускает сервер и открывает порт на поддерживаемом роутере. Если macOS спросит о входящих подключениях, разрешите их. Перед подключением все открывают один и тот же видеофайл.</p>
  </>:status==='disconnected'?<>
   <p role="alert">Соединение с комнатой потеряно</p>
   <button onClick={()=>void run(()=>api.watchLeave(),'Комната закрыта')}>Закрыть комнату</button>
  </>:<>
   <p role="status">{status==='host'?'Вы ведущий комнаты':'Вы подключены к комнате'}</p>
   {status==='host'&&share&&<><textarea aria-label="Ссылка-приглашение" readOnly value={share}/><button onClick={()=>void copy()}>Скопировать приглашение</button></>}
   <button onClick={()=>void run(()=>api.watchLeave(),'Вы вышли из комнаты')}>Завершить совместный просмотр</button>
  </>}
  {message&&<p className="room-help" role="status">{message}</p>}
 </section>;
}
export default function App(){
 const [view,setView]=useState(initialView),[shown,setShown]=useState(true),[error,setError]=useState(''),[choosing,setChoosing]=useState(false),[full,setFull]=useState(false);
 const selection=useRef(0),accepted=useRef(0);
 useEffect(()=>{
  let disposed=false;let unlisten:(()=>void)|undefined;const serial=selection.current;
  void api.listen(next=>{if(!disposed)setView(current=>acceptEvent(current,next));}).then(stop=>{if(disposed)stop();else unlisten=stop;}).catch(()=>{if(!disposed)setError('Связь с плеером недоступна');});
  void api.snapshot().then(next=>{if(!disposed&&selection.current===serial)setView(next);}).catch(()=>{if(!disposed)setError('Плеер недоступен');});
  return ()=>{disposed=true;unlisten?.();};
 },[]);
 async function choose(){const serial=++selection.current;setChoosing(true);setError('');try{const next=await api.choose();if(next&&serial>=accepted.current){accepted.current=serial;setView(next);}}catch{if(serial===selection.current){setError('Не удалось открыть фильм');try{const next=await api.snapshot();if(serial===selection.current)setView(next);}catch{/* stable public text only */}}}finally{if(serial===selection.current)setChoosing(false);}}
 async function send(command:Command){try{if(await api.watchStatus()!=='idle'){
   let action:WatchAction|undefined;
   if(command.type==='SetPaused')action=command.value?{type:'pause'}:{type:'play'};
   if(command.type==='SeekToMs')action={type:'seek',positionMs:command.value};
   if(command.type==='SetSpeed')action={type:'setSpeed',speed:command.value};
   if(action){await api.watchCommand(action);setError('');return;}
  }await api.command(view.generation,command);setError('');}catch{setError('Команда недоступна');}}
 async function fullscreen(){try{await api.fullscreen(!full);setFull(!full);}catch{setError('Полноэкранный режим недоступен');}}
 return <main className={view.unavailable?'watch unavailable':'watch'}>
  <header><div><h1>{view.basename??'Branch'}</h1><p>Локальный просмотр</p></div><button disabled={view.unavailable} aria-busy={choosing} onClick={()=>void choose()}>Открыть фильм</button></header>
  {(view.unavailable||!view.basename)&&<div className="empty"><p>{view.unavailable?'Нативная видеоповерхность недоступна на этой ОС':'Выберите MP4, MOV или MKV'}</p></div>}
  <div className="proof"><span role="status">{statuses[view.status]}</span><button aria-expanded={shown} onClick={()=>setShown(!shown)}>{shown?'Скрыть панель':'Показать панель'}</button></div>
  {error&&<p className="error" role="alert">{error}</p>}
  {shown&&<PlayerPanel view={view} send={send} fullscreen={()=>void fullscreen()}/>}
  <WatchPanel enabled={!!view.basename&&!view.unavailable} clearError={()=>setError('')}/>
 </main>;
}
