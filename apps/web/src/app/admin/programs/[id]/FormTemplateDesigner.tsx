"use client";
import {useCallback,useEffect,useRef,useState} from 'react';
import {adminApi,ApiError,type AdminFormTemplateView,type FormFieldType,type FormTemplateField,type FormTemplateGroup} from '@/lib/api';

/*
 * 서류 작성 도우미 template designer. The admin opens the verified official
 * PDF from their own disk (it must hash to the reviewed content_hash), draws
 * input boxes once, and users then fill those boxes on their own device.
 * Nothing here uploads the PDF; only box positions are saved.
 */
const PDFJS='https://cdn.jsdelivr.net/npm/pdfjs-dist@3.11.174/build';
const SCRIPTS:[string,string][]=[
  [`${PDFJS}/pdf.worker.min.js`,'sha384-SnzOobpRMLXZ52iJvZm/C0fYw0OQemTXzTjIsdsfMcrCtCEe9qgzxTd3RSklO5x2'],
  [`${PDFJS}/pdf.min.js`,'sha384-/1qUCSGwTur9vjf/z9lmu/eCUYbpOTgSjmpbMQZ1/CtX2v/WcAIKqRv+U1DUCG6e'],
];
type Viewport={width:number;height:number};
type PdfAnnotation={subtype:string;fieldType?:string;fieldName?:string;alternativeText?:string;rect:number[];checkBox?:boolean;radioButton?:boolean;pushButton?:boolean;multiLine?:boolean;required?:boolean;buttonValue?:string};
type PdfPage={view:number[];rotate:number;getViewport:(o:{scale:number})=>Viewport;render:(o:{canvasContext:CanvasRenderingContext2D;viewport:Viewport})=>{promise:Promise<void>};getAnnotations:()=>Promise<PdfAnnotation[]>};
type PdfDoc={numPages:number;getPage:(n:number)=>Promise<PdfPage>;destroy:()=>Promise<void>};
type PdfJs={getDocument:(o:{data:Uint8Array;isEvalSupported:boolean;cMapUrl:string;cMapPacked:boolean})=>{promise:Promise<PdfDoc>}};

function loadScript(src:string,integrity:string):Promise<void> {
  return new Promise((resolve,reject)=>{
    const existing=document.querySelector<HTMLScriptElement>(`script[data-pdfjs="${src}"]`);
    if(existing?.dataset.loaded==='1'){resolve();return;}
    const s=existing??document.createElement('script');
    s.addEventListener('load',()=>{s.dataset.loaded='1';resolve();});
    s.addEventListener('error',()=>reject(new Error('PDF 표시 프로그램을 불러오지 못했습니다.')));
    if(!existing){s.src=src;s.integrity=integrity;s.crossOrigin='anonymous';s.dataset.pdfjs=src;document.head.appendChild(s);}
  });
}
async function pdfjs():Promise<PdfJs> {
  // The worker script runs on the main thread (pdf.js "fake worker"), so no cross-origin Worker is needed.
  for(const [src,integrity] of SCRIPTS)await loadScript(src,integrity);
  return (window as unknown as {pdfjsLib:PdfJs}).pdfjsLib;
}
async function sha256(bytes:ArrayBuffer):Promise<string> {
  return Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',bytes)),b=>b.toString(16).padStart(2,'0')).join('');
}

const TYPES:Record<FormFieldType,string>={text:'한 줄 글자',multiline:'여러 줄 글자',date:'날짜',checkbox:'체크',signature:'서명',stamp:'도장(이름)'};
const MAPPINGS:Record<string,string>={name:'성명',name_en:'영문 성명',birth_date:'생년월일',phone:'연락처',address:'주소',email:'이메일',school:'학교명',major:'학과',grade:'학년',student_id:'학번',application_date:'신청일',bank:'은행명',account_holder:'예금주 (작성 시에만 입력, 제안 안 함)',rrn:'주민등록번호 (작성 시에만 입력, 제안 안 함)',account:'계좌번호 (작성 시에만 입력, 제안 안 함)',signature:'서명',stamp:'도장'};
const DATE_FORMATS=['YYYY.MM.DD','YYYY-MM-DD','YYYY. MM. DD.','YYYY년 MM월 DD일','YYMMDD'];
const control='rounded border border-gray-300 p-1 text-sm';
type Rect=FormTemplateField['rect'];
const clamp=(v:number)=>Math.min(1,Math.max(0,v));
const round=(v:number)=>Math.round(v*10000)/10000;
let seq=0;
const newId=()=>`f-${Date.now().toString(36)}-${(seq++).toString(36)}`;

function errorMessage(error:unknown):string {
  if(error instanceof ApiError){
    if(error.status===409)return '양식 파일 또는 정의가 그사이 바뀌었습니다. 새로고침한 뒤 다시 저장하세요.';
    if(error.status===401||error.status===403)return '관리자 권한을 다시 확인하세요.';
    if(error.body&&typeof error.body==='object'&&'error' in error.body&&typeof error.body.error==='string')return error.body.error;
    return '저장하지 못했습니다. 입력 칸 정보를 확인하세요.';
  }
  return error instanceof Error?error.message:'서버에 연결하지 못했습니다.';
}

/** pdf.js widget rect (user space) → fractions of the page view box, top-left origin. */
function toFraction(rect:number[],view:number[]):Rect|null {
  const [vx1,vy1,vx2,vy2]=view,W=vx2-vx1,H=vy2-vy1;
  const [x1,y1,x2,y2]=[Math.min(rect[0],rect[2]),Math.min(rect[1],rect[3]),Math.max(rect[0],rect[2]),Math.max(rect[1],rect[3])];
  const x=clamp((x1-vx1)/W),right=clamp((x2-vx1)/W),top=clamp((vy2-y2)/H),bottom=clamp((vy2-y1)/H);
  const r={x:round(x),y:round(top),w:round(right-x),h:round(bottom-top)};
  return r.w>=0.005&&r.h>=0.005?r:null;
}

export default function FormTemplateDesigner({resourceId}:{resourceId:string}) {
  const [view,setView]=useState<AdminFormTemplateView|null>(null);
  const [fields,setFields]=useState<FormTemplateField[]>([]);
  const [groups,setGroups]=useState<FormTemplateGroup[]>([]);
  const [pages,setPages]=useState<{url:string;aspect:number}[]>([]);
  const [hash,setHash]=useState<string|null>(null);
  const [widgets,setWidgets]=useState<FormTemplateField[]>([]);
  const [selected,setSelected]=useState<string|null>(null);
  const [draft,setDraft]=useState<{page:number;start:[number,number];rect:Rect}|null>(null);
  const [rotated,setRotated]=useState(false);
  const [busy,setBusy]=useState(false),[error,setError]=useState(''),[notice,setNotice]=useState('');
  const doc=useRef<PdfDoc|null>(null);

  const load=useCallback(async()=>{const v=await adminApi.getFormTemplate(resourceId);setView(v);if(v.template){setFields(v.template.fields);setGroups(v.template.groups??[]);}},[resourceId]);
  useEffect(()=>{let live=true;adminApi.getFormTemplate(resourceId).then(v=>{if(!live)return;setView(v);if(v.template){setFields(v.template.fields);setGroups(v.template.groups??[]);}}).catch(e=>{if(live)setError(errorMessage(e));});return()=>{live=false;void doc.current?.destroy();};},[resourceId]);

  async function openFile(file:File|undefined) {
    if(!file||!view)return;
    setBusy(true);setError('');setNotice('');
    try {
      const bytes=await file.arrayBuffer();
      const digest=await sha256(bytes);
      if(digest!==view.resource.content_hash)throw new Error('검토된 원본과 다른 파일입니다. "자료 열기"로 받은 공식 파일을 그대로 선택하세요.');
      const lib=await pdfjs();
      await doc.current?.destroy();
      const pdf=await lib.getDocument({data:new Uint8Array(bytes),isEvalSupported:false,cMapUrl:'https://cdn.jsdelivr.net/npm/pdfjs-dist@3.11.174/cmaps/',cMapPacked:true}).promise;
      doc.current=pdf;
      if(pdf.numPages>50)throw new Error('50쪽을 넘는 양식은 지원하지 않습니다.');
      const rendered:{url:string;aspect:number}[]=[],found:FormTemplateField[]=[];let anyRotated=false;
      for(let i=1;i<=pdf.numPages;i++){
        const page=await pdf.getPage(i);
        if(page.rotate%360!==0)anyRotated=true;
        const base=page.getViewport({scale:1}),viewport=page.getViewport({scale:1000/base.width});
        const canvas=document.createElement('canvas');canvas.width=Math.round(viewport.width);canvas.height=Math.round(viewport.height);
        const ctx=canvas.getContext('2d');if(!ctx)throw new Error('캔버스를 만들 수 없습니다.');
        ctx.fillStyle='#fff';ctx.fillRect(0,0,canvas.width,canvas.height);
        await page.render({canvasContext:ctx,viewport}).promise;
        rendered.push({url:canvas.toDataURL('image/png'),aspect:base.width/base.height});
        for(const a of await page.getAnnotations()){
          if(a.subtype!=='Widget'||a.pushButton)continue;
          const type:FormFieldType|null=a.fieldType==='Tx'?(a.multiLine?'multiline':'text'):a.fieldType==='Btn'?'checkbox':a.fieldType==='Sig'?'signature':a.fieldType==='Ch'?'text':null;
          const rect=type&&toFraction(a.rect,page.view);
          if(!type||!rect)continue;
          const name=(a.fieldName??'').slice(0,200);
          const label=((a.alternativeText||name.split('.').pop()||'입력 칸')+(a.radioButton&&a.buttonValue?` · ${a.buttonValue}`:'')).slice(0,80);
          found.push({id:newId(),page:i-1,rect,type,label,mapping:null,required:a.radioButton?false:!!a.required,acro_name:name||undefined});
        }
      }
      setPages(rendered);setHash(digest);setWidgets(found);setRotated(anyRotated);
      if(view.template&&view.template.page_count!==pdf.numPages){setNotice('저장된 정의와 쪽수가 다릅니다. 칸 위치를 모두 다시 확인하세요.');}
      if(!fields.length&&found.length)setNotice(`양식 파일에 입력 칸 ${found.length}개가 있습니다. "양식 입력 칸 가져오기"로 시작할 수 있어요.`);
    } catch(e){setError(errorMessage(e));setPages([]);setHash(null);}
    finally{setBusy(false);}
  }

  const sel=fields.find(f=>f.id===selected)??null;
  const patch=(id:string,p:Partial<FormTemplateField>)=>setFields(fs=>fs.map(f=>f.id===id?{...f,...p}:f));
  // Numeric edit: clamp the position first, then cap the size to the page.
  const patchRect=(id:string,r:Partial<Rect>)=>setFields(fs=>fs.map(f=>{if(f.id!==id)return f;const n={...f.rect,...r};n.x=round(Math.min(clamp(n.x),0.995));n.y=round(Math.min(clamp(n.y),0.995));n.w=round(Math.max(0.005,Math.min(n.w,1-n.x)));n.h=round(Math.max(0.005,Math.min(n.h,1-n.y)));return {...f,rect:n};}));
  // Keyboard move: keep the size, stop at the page edge.
  const moveRect=(id:string,dx:number,dy:number)=>setFields(fs=>fs.map(f=>f.id!==id?f:{...f,rect:{...f.rect,x:round(Math.min(Math.max(0,f.rect.x+dx),1-f.rect.w)),y:round(Math.min(Math.max(0,f.rect.y+dy),1-f.rect.h))}}));

  function point(e:React.PointerEvent<HTMLDivElement>):[number,number] {
    const box=e.currentTarget.getBoundingClientRect();
    return [clamp((e.clientX-box.left)/box.width),clamp((e.clientY-box.top)/box.height)];
  }
  function onPointerDown(page:number,e:React.PointerEvent<HTMLDivElement>) {
    if(e.target!==e.currentTarget)return;
    e.currentTarget.setPointerCapture(e.pointerId);
    const p=point(e);setDraft({page,start:p,rect:{x:p[0],y:p[1],w:0,h:0}});setSelected(null);
  }
  function onPointerMove(e:React.PointerEvent<HTMLDivElement>) {
    if(!draft)return;const [x,y]=point(e),[sx,sy]=draft.start;
    setDraft({...draft,rect:{x:Math.min(x,sx),y:Math.min(y,sy),w:Math.abs(x-sx),h:Math.abs(y-sy)}});
  }
  function onPointerUp() {
    if(!draft)return;
    const r=draft.rect;setDraft(null);
    if(r.w<0.005||r.h<0.005)return;
    const field:FormTemplateField={id:newId(),page:draft.page,rect:{x:round(r.x),y:round(r.y),w:round(r.w),h:round(r.h)},type:'text',label:`입력 칸 ${fields.length+1}`,mapping:null,required:false};
    setFields(fs=>[...fs,field]);setSelected(field.id);
  }
  function onKeyDown(e:React.KeyboardEvent) {
    if(!sel||(e.target as HTMLElement).tagName.match(/INPUT|SELECT|TEXTAREA/))return;
    const step=e.shiftKey?0.01:0.002;
    const moves:Record<string,[number,number]>={ArrowLeft:[-step,0],ArrowRight:[step,0],ArrowUp:[0,-step],ArrowDown:[0,step]};
    if(moves[e.key]){e.preventDefault();moveRect(sel.id,...moves[e.key]);}
    if(e.key==='Delete'||e.key==='Backspace'){e.preventDefault();setFields(fs=>fs.filter(f=>f.id!==sel.id));setSelected(null);}
  }

  async function save() {
    if(!hash||!pages.length)return;
    setBusy(true);setError('');setNotice('');
    try {
      const unlabeled=fields.find(f=>!f.label.trim());
      if(unlabeled)throw new Error('모든 입력 칸에 이름을 붙여 주세요.');
      // Same rules as the server: an unused group is dropped, a used one needs 2+ checkboxes on one page.
      const used=groups.filter(g=>fields.some(f=>f.group===g.id));
      for(const g of used){
        const options=fields.filter(f=>f.group===g.id);
        if(!g.label.trim())throw new Error('모든 선택 묶음에 이름을 붙여 주세요.');
        if(options.length<2||options.some(f=>f.page!==options[0].page||f.type!=='checkbox'))throw new Error(`선택 묶음 "${g.label}"에는 같은 쪽의 체크 칸이 2개 이상 있어야 합니다.`);
      }
      const saved=await adminApi.saveFormTemplate(resourceId,{version:1,content_hash:hash,page_count:pages.length,source:'admin',fields:fields.map(f=>({...f,label:f.label.trim()})),...(used.length?{groups:used.map(g=>({...g,label:g.label.trim()}))}:{})},view?.revision??null);
      await load();setNotice(`저장했습니다 (버전 ${saved.revision}). 앱에서 이 양식의 입력 칸으로 바로 쓰입니다.`);
    } catch(e){setError(errorMessage(e));}
    finally{setBusy(false);}
  }
  async function remove() {
    if(!window.confirm('이 양식의 입력 칸 정의를 삭제할까요? 앱은 안내형 작성으로 돌아갑니다.'))return;
    setBusy(true);setError('');
    try{await adminApi.deleteFormTemplate(resourceId);setFields([]);await load();setNotice('삭제했습니다.');}catch(e){setError(errorMessage(e));}finally{setBusy(false);}
  }

  if(!view)return <p className="text-sm text-gray-600">{error||'입력 칸 정의를 불러오는 중…'}</p>;
  return <div className="space-y-3 rounded border border-teal-200 bg-teal-50/40 p-3" onKeyDown={onKeyDown} tabIndex={-1}>
    <p className="text-xs text-gray-600">앱의 서류 작성 도우미가 사용할 입력 칸을 그립니다. 이용자 입력 값은 서버에 저장되지 않고, 여기서는 칸 위치·종류·연결 정보만 저장합니다. 주민등록번호·계좌번호 칸은 앱이 제안하지 않고 작성 시에만 입력받습니다.</p>
    {view.stale&&<p role="alert" className="rounded bg-amber-50 p-2 text-sm text-amber-900">공식 양식 파일이 바뀌어 저장된 정의({view.template?.fields.length}칸)는 앱에 제공되지 않습니다. 새 파일을 열어 위치를 확인한 뒤 다시 저장하세요.</p>}
    {view.template&&!view.stale&&<p className="text-sm">저장된 정의: {view.template.fields.length}칸 · 버전 {view.revision} · {view.updated_at&&new Date(view.updated_at).toLocaleString('ko-KR')}</p>}
    <label className="block text-sm">검토된 원본 PDF 선택 <input type="file" accept="application/pdf,.pdf" disabled={busy} onChange={e=>void openFile(e.target.files?.[0])} className="block text-sm"/></label>
    {rotated&&<p role="alert" className="text-sm text-red-700">회전된 쪽이 있는 PDF입니다. 앱은 이 양식을 칸 입력 대신 안내형으로 보여줍니다.</p>}
    {!!pages.length&&<div className="flex flex-wrap gap-2 text-sm">
      {!!widgets.length&&<button type="button" className="rounded border px-3 py-1" onClick={()=>{setFields(widgets.map(w=>({...w,id:newId()})));setNotice(`${widgets.length}개 칸을 가져왔습니다. 이름과 연결 정보를 확인하세요.`);}}>양식 입력 칸 가져오기 ({widgets.length})</button>}
      <button type="button" disabled={busy||!fields.length} onClick={()=>void save()} className="rounded bg-teal-700 px-3 py-1 text-white disabled:opacity-50">{busy?'저장 중…':`입력 칸 ${fields.length}개 저장`}</button>
      {view.template&&<button type="button" disabled={busy} onClick={()=>void remove()} className="rounded border px-3 py-1 text-red-700">정의 삭제</button>}
      <span className="text-xs text-gray-500">빈 곳을 끌어서 칸을 만들고, 칸을 눌러 선택하세요. 방향키로 이동(Shift: 크게), Delete로 삭제.</span>
    </div>}
    {notice&&<p className="text-sm text-teal-800">{notice}</p>}
    {error&&<p role="alert" className="text-sm text-red-700">{error}</p>}
    {!!pages.length&&<div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_280px]">
      <div className="space-y-4">{pages.map((p,page)=><div key={page}>
        <p className="text-xs text-gray-500">{page+1}쪽</p>
        <div className="relative w-full max-w-[800px] touch-none select-none border bg-white" style={{aspectRatio:String(p.aspect),backgroundImage:`url(${p.url})`,backgroundSize:'100% 100%'}}
          onPointerDown={e=>onPointerDown(page,e)} onPointerMove={onPointerMove} onPointerUp={onPointerUp}>
          {fields.filter(f=>f.page===page).map(f=><button type="button" key={f.id} title={f.label} onPointerDown={e=>{e.stopPropagation();setSelected(f.id);}}
            className={`absolute overflow-hidden border text-left text-[10px] leading-tight ${f.id===selected?'border-2 border-indigo-700 bg-indigo-200/40':f.required?'border-orange-500 bg-orange-200/30':'border-teal-600 bg-teal-200/30'}`}
            style={{left:`${f.rect.x*100}%`,top:`${f.rect.y*100}%`,width:`${f.rect.w*100}%`,height:`${f.rect.h*100}%`}}>{f.label}</button>)}
          {draft?.page===page&&<div className="pointer-events-none absolute border-2 border-dashed border-indigo-600" style={{left:`${draft.rect.x*100}%`,top:`${draft.rect.y*100}%`,width:`${draft.rect.w*100}%`,height:`${draft.rect.h*100}%`}}/>}
        </div>
      </div>)}</div>
      <aside className="space-y-3 text-sm lg:sticky lg:top-4 lg:self-start">
        {sel?<div className="space-y-2 rounded border bg-white p-3">
          <label className="block">이름<input value={sel.label} maxLength={80} onChange={e=>patch(sel.id,{label:e.target.value})} className={`${control} w-full`}/></label>
          <label className="block">종류<select value={sel.type} onChange={e=>{const type=e.target.value as FormFieldType;patch(sel.id,type==='checkbox'?{type}:{type,group:undefined});}} className={`${control} w-full`}>{Object.entries(TYPES).map(([v,t])=><option key={v} value={v}>{t}</option>)}</select></label>
          <label className="block">연결 정보<select value={sel.mapping??''} onChange={e=>patch(sel.id,{mapping:e.target.value||null})} className={`${control} w-full`}><option value="">없음 (이용자가 직접 입력)</option>{Object.entries(MAPPINGS).map(([v,t])=><option key={v} value={v}>{t}</option>)}</select></label>
          {sel.type==='date'&&<label className="block">날짜 표기<select value={sel.format??'YYYY.MM.DD'} onChange={e=>patch(sel.id,{format:e.target.value})} className={`${control} w-full`}>{DATE_FORMATS.map(f=><option key={f}>{f}</option>)}</select></label>}
          {sel.type==='checkbox'&&<div className="space-y-1 rounded border border-dashed p-2">
            <label className="block">선택 묶음<select value={sel.group??''} onChange={e=>{
              if(e.target.value==='__new'){let n=groups.length+1;while(groups.some(g=>g.id===`g${n}`))n++;const id=`g${n}`;setGroups(gs=>[...gs,{id,mode:'single',label:sel.label.split(':')[0].trim().slice(0,80)||'선택'}]);patch(sel.id,{group:id});}
              else patch(sel.id,{group:e.target.value||undefined});
            }} className={`${control} w-full`}><option value="">없음 (따로 체크)</option>{groups.map(g=><option key={g.id} value={g.id}>{g.label} ({g.mode==='single'?'하나만':'여러 개'})</option>)}<option value="__new">+ 새 묶음</option></select></label>
            {(()=>{const g=groups.find(x=>x.id===sel.group);if(!g)return null;const set=(p:Partial<FormTemplateGroup>)=>setGroups(gs=>gs.map(x=>x.id===g.id?{...x,...p}:x));return <>
              <label className="block">묶음 이름<input value={g.label} maxLength={80} onChange={e=>set({label:e.target.value})} className={`${control} w-full`}/></label>
              <fieldset className="flex gap-3"><legend className="sr-only">선택 방식</legend>
                <label><input type="radio" name={`mode-${g.id}`} checked={g.mode==='single'} onChange={()=>set({mode:'single'})}/> 하나만 선택</label>
                <label><input type="radio" name={`mode-${g.id}`} checked={g.mode==='multi'} onChange={()=>set({mode:'multi'})}/> 여러 개 선택</label>
              </fieldset>
              <p className="text-xs text-gray-500">선택지 {fields.filter(f=>f.group===g.id).length}개 · 같은 쪽의 체크 칸 2개 이상이어야 저장됩니다.</p>
            </>;})()}
          </div>}
          <label className="block"><input type="checkbox" checked={sel.required} onChange={e=>patch(sel.id,{required:e.target.checked})}/> 필수 항목</label>
          <label className="block">글자 크기(pt, 비우면 자동)<input type="number" min={5} max={24} value={sel.font_size??''} onChange={e=>patch(sel.id,{font_size:e.target.value?Number(e.target.value):undefined})} className={`${control} w-full`}/></label>
          <div className="grid grid-cols-2 gap-1">{(['x','y','w','h'] as const).map(k=><label key={k} className="block text-xs">{({x:'왼쪽',y:'위',w:'너비',h:'높이'})[k]} %<input type="number" step={0.1} value={Math.round(sel.rect[k]*1000)/10} onChange={e=>patchRect(sel.id,{[k]:Number(e.target.value)/100})} className={`${control} w-full`}/></label>)}</div>
          <button type="button" onClick={()=>{setFields(fs=>fs.filter(f=>f.id!==sel.id));setSelected(null);}} className="text-red-700">이 칸 삭제</button>
        </div>:<p className="text-gray-600">칸을 선택하면 이름·종류·연결 정보를 고칠 수 있어요.</p>}
        <ol className="max-h-80 space-y-1 overflow-auto rounded border bg-white p-2">{fields.map(f=><li key={f.id}><button type="button" onClick={()=>setSelected(f.id)} className={`w-full text-left ${f.id===selected?'font-semibold text-indigo-800':''}`}>{f.page+1}쪽 · {f.label}{f.required?' *':''} <span className="text-xs text-gray-500">{TYPES[f.type]}{f.mapping?` → ${MAPPINGS[f.mapping]?.split(' (')[0]??f.mapping}`:''}</span></button></li>)}</ol>
      </aside>
    </div>}
  </div>;
}
