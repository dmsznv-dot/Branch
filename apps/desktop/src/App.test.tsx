import { render,screen } from '@testing-library/react';
import { describe,it,expect } from 'vitest';
import { PlayerPanel } from './App';
import { acceptEvent,initialView } from './player';
describe('metadata_is_plain_text',()=>{
 it('renders untrusted basename literally and unknown duration as dash',()=>{
  const basename='<img src=x onerror=alert(1)>';
  const {container}=render(<PlayerPanel view={{...initialView,generation:'A',basename}}/>);
  expect(screen.getByText(basename)).toBeInTheDocument();
  expect(container.querySelector('img')).toBeNull();
  expect(screen.getByTestId('duration')).toHaveTextContent('—');
  expect(screen.getByRole('slider',{name:'Позиция'})).toBeDisabled();
 });
 it('ignores generation A event after selection B',()=>{
  const b={...initialView,generation:'B',basename:'B.mkv'};
  expect(acceptEvent(b,{...initialView,generation:'A',basename:'A.mkv'})).toBe(b);
 });
});

import { vi } from 'vitest';
import App from './App';
import { api,togglePlayback } from './player';
it('restarts Ended with seek zero before play',async()=>{
 const commands:unknown[]=[];
 await togglePlayback({...initialView,status:'Ended'},async command=>{commands.push(command);});
 expect(commands).toEqual([{type:'SeekToMs',value:'0'},{type:'SetPaused',value:false}]);
});
it('keeps a selected film when native chooser is cancelled and accepts controls focus',async()=>{
 const {fireEvent,waitFor}=await import('@testing-library/react');
 const existing={...initialView,generation:'B',basename:'B.mkv',duration_ms:'60000',status:'Paused' as const};
 const snapshot=vi.spyOn(api,'snapshot').mockResolvedValue(existing);
 const listen=vi.spyOn(api,'listen').mockResolvedValue(()=>{});
 const choose=vi.spyOn(api,'choose').mockResolvedValue(null);
 const command=vi.spyOn(api,'command').mockResolvedValue();
 render(<App/>);
 await screen.findByRole('heading',{name:'B.mkv'});
 fireEvent.click(screen.getByRole('button',{name:'Открыть фильм'}));
 await waitFor(()=>expect(choose).toHaveBeenCalledOnce());
 expect(screen.getByRole('heading',{name:'B.mkv'})).toBeInTheDocument();
 expect(command).not.toHaveBeenCalled();
 const toggle=screen.getByRole('button',{name:'Скрыть панель'});toggle.focus();expect(toggle).toHaveFocus();
 fireEvent.click(toggle);expect(screen.queryByRole('slider',{name:'Позиция'})).toBeNull();
 fireEvent.click(screen.getByRole('button',{name:'Показать панель'}));expect(screen.getByRole('slider',{name:'Позиция'})).toBeEnabled();
 snapshot.mockRestore();listen.mockRestore();choose.mockRestore();command.mockRestore();
});
it('allows choosing B while A loads and ignores A completing after B',async()=>{
 const {fireEvent}=await import('@testing-library/react');
 const {act}=await import('react');
 let aResolve!:(view:typeof initialView)=>void,bResolve!:(view:typeof initialView)=>void;
 const a=new Promise<typeof initialView>(resolve=>{aResolve=resolve;}),b=new Promise<typeof initialView>(resolve=>{bResolve=resolve;});
 const snapshot=vi.spyOn(api,'snapshot').mockResolvedValue(initialView);
 const listen=vi.spyOn(api,'listen').mockResolvedValue(()=>{});
 const choose=vi.spyOn(api,'choose').mockReturnValueOnce(a).mockReturnValueOnce(b);
 render(<App/>);
 const open=screen.getByRole('button',{name:'Открыть фильм'});
 fireEvent.click(open);fireEvent.click(open);
 expect(choose).toHaveBeenCalledTimes(2);
 await act(async()=>{bResolve({...initialView,generation:'B',basename:'B.mkv'});await b;});
 await act(async()=>{aResolve({...initialView,generation:'A',basename:'A.mkv'});await a;});
 expect(screen.getByRole('heading',{name:'B.mkv'})).toBeInTheDocument();
 snapshot.mockRestore();listen.mockRestore();choose.mockRestore();
});
