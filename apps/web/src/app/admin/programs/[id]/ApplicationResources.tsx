"use client";
import {useCallback,useEffect,useState} from 'react';
import {adminApi,ApiError,type AdminApplicationResource,type ApplicationResourceReviewList} from '@/lib/api';
import FormTemplateDesigner from './FormTemplateDesigner';

const control='w-full rounded border border-gray-300 p-2 text-sm';
const kindName={notice:'공고 원문',application:'실제 신청처',template:'제출 양식'};
const stateName:Record<string,string>={candidate:'검토 대기',verified:'검토 승인',rejected:'제외',unavailable:'재확인 필요'};
function errorMessage(error:unknown):string {
  if(error instanceof ApiError) {
    if(error.status===409)return '공고·자료 또는 검증 상태가 변경됐습니다. 목록을 새로고침한 뒤 다시 확인하세요.';
    if(error.status===401||error.status===403)return '관리자 권한을 다시 확인하세요.';
    if((error.status===400||error.status===412)&&error.body&&typeof error.body==='object'&&'error' in error.body&&typeof error.body.error==='string')return error.body.error;
    return '요청을 처리하지 못했습니다. 잠시 후 다시 시도하세요.';
  }
  return '서버에 연결하지 못했습니다. 네트워크를 확인하세요.';
}
// Every code the worker or a review can write to check_error / last_failure_code.
const errorName:Record<string,string>={approval_required:'출처 도메인·경로의 수집 승인이 필요합니다.',robots_unavailable_or_disallowed:'robots 확인 실패 또는 수집 불허',domain_cooldown:'요청 제한 대기',unsafe_url_or_dns:'안전하지 않은 주소 또는 DNS',redirect_requires_review:'주소가 이동됩니다. 최종 주소를 별도 확인하세요.',source_notice_redirect_requires_review:'근거 공고 주소가 이동됩니다. 새 공고 주소를 확인하세요.',network_timeout_or_failure:'일시적인 통신 실패',response_too_large_or_unreadable:'파일 크기 제한 또는 읽기 실패',unsupported_file_or_page:'지원하지 않는 파일·페이지 형식',database_error:'수집기 내부 오류',superseded:'다른 신청처가 승인되어 대체됨'};
function errorText(code:string):string {
  const http=/^source_http_(\d{3})$/.exec(code);
  if(http)return http[1]==='404'||http[1]==='410'?`페이지 없음(HTTP ${http[1]}). 3회 연속이면 승인이 해제됩니다.`:`기관 서버 응답 오류(HTTP ${http[1]})`;
  return errorName[code]??code;
}
const checkName:Record<string,string>={ok:'정상',blocked:'수집기 접근 불가(승인·robots·이동)',unreachable:'응답 없음·HTTP 오류',changed:'검토 이후 내용 변경됨, 다시 확인 필요',unsupported:'지원하지 않는 형식'};
type Row=AdminApplicationResource&{check_status?:string|null;consecutive_failures?:number;last_failure_code?:string|null};
function hostOf(url:string):string {try{return new URL(url).hostname;}catch{return '주소 해석 실패';}}

export default function ApplicationResources({programId}:{programId:string}) {
  const [data,setData]=useState<ApplicationResourceReviewList|null>(null);
  const [error,setError]=useState(''); const [busy,setBusy]=useState(false);
  const load=useCallback(async()=>{setData(await adminApi.listApplicationResources(programId));},[programId]);
  useEffect(()=>{let live=true;adminApi.listApplicationResources(programId).then(d=>{if(live)setData(d);}).catch(()=>{if(live)setError('신청 자료를 불러오지 못했습니다.');});return()=>{live=false;};},[programId]);
  async function add(event:React.FormEvent<HTMLFormElement>) {
    event.preventDefault();const form=event.currentTarget;const values=new FormData(form);setBusy(true);setError('');
    try{await adminApi.queueApplicationResource(programId,{kind:String(values.get('kind')),url:String(values.get('url')),source_url:String(values.get('source')),label:String(values.get('label'))});form.reset();await load();}
    catch(e){setError(errorMessage(e));}finally{setBusy(false);}
  }
  return <section className="space-y-4 rounded-xl border bg-white p-6">
    <h2 className="text-lg font-semibold">신청처 · 공식 양식 검토</h2>
    <p className="text-sm text-gray-600">주소 후보는 자동 공개되지 않습니다. 사업·모집 회차·실제 화면을 확인하고 승인하세요. 로그인 필요 주소는 로그인 이후 화면 확인과 구분합니다. 파일은 원본 링크와 해시만 관리하며 재배포하지 않습니다.</p>
    {error&&<p role="alert" className="text-sm text-red-700">{error}</p>}
    <button type="button" className="rounded border px-3 py-2 text-sm" onClick={()=>void load().catch(()=>setError('새로고침 실패'))}>검토 목록 새로고침</button>
    {data?.items.map((row:Row)=><ReviewCard key={`${row.id}:${row.revision}:${data.program_fingerprint}`} row={row} fingerprint={data.program_fingerprint} reload={load}/>)}
    {data&&!data.items.length&&<p className="text-sm">등록된 후보가 없습니다. 워커 수집을 실행하거나 아래에서 공식 근거와 함께 등록하세요.</p>}
    <form onSubmit={add} className="space-y-3 border-t pt-4">
      <label className="block text-sm">자료 종류<select name="kind" className={control}>{Object.entries(kindName).map(([v,t])=><option key={v} value={v}>{t}</option>)}</select></label>
      <label className="block text-sm">표시 이름<input name="label" required maxLength={200} className={control}/></label>
      <label className="block text-sm">자료 주소<input name="url" type="url" required maxLength={2048} placeholder="https://" className={control}/></label>
      <label className="block text-sm">공식 근거 공고 주소<input name="source" type="url" required maxLength={2048} placeholder="https://" className={control}/></label>
      <button disabled={busy} className="rounded bg-teal-700 px-4 py-2 text-white disabled:opacity-50">{busy?'등록 중…':'후보 등록'}</button>
    </form>
  </section>;
}

function ReviewCard({row,fingerprint,reload}:{row:Row;fingerprint:string;reload:()=>Promise<void>}) {
  const [same,setSame]=useState(false),[intake,setIntake]=useState(false),[destination,setDestination]=useState(false);
  const [access,setAccess]=useState('direct'),[note,setNote]=useState(''),[hours,setHours]=useState(24);
  const [busy,setBusy]=useState(false),[error,setError]=useState(''),[designing,setDesigning]=useState(false);
  // The card remounts on every reload (its key carries the revision), so the
  // clock is read once per load, not during each render.
  const [mountedAt]=useState(()=>Date.now());
  const expired=!!row.valid_until&&Date.parse(row.valid_until)<=mountedAt;
  const host=hostOf(row.url),idn=host.split('.').some(label=>label.startsWith('xn--'));
  async function review(decision:string) {setBusy(true);setError('');try{await adminApi.reviewApplicationResource(row.id,{decision,expected_revision:row.revision,program_fingerprint:fingerprint,access,same_program_confirmed:same,current_intake_confirmed:intake,destination_confirmed:destination,note,validity_hours:hours});await reload();}catch(e){setError(errorMessage(e));}finally{setBusy(false);}}
  return <article className="space-y-2 rounded-lg border p-4 text-sm">
    <h3 className="font-semibold">{kindName[row.kind]} · {row.label}</h3>
    <p>{stateName[row.status]??row.status}{expired?' · 검증 기한 만료':''}{row.program_fingerprint!==fingerprint?' · 공고 변경됨':''}</p>
    <p className="text-base">접속 호스트: <strong className="break-all font-mono">{host}</strong></p>
    {idn&&<p role="alert" className="rounded bg-red-50 p-2 font-semibold text-red-800">국제화 도메인(xn--)입니다. 기관 이름을 흉내 낸 유사 주소일 수 있어요. 승인된 출처 도메인으로 등록된 경우에만 승인됩니다.</p>}
    <a href={row.url} target="_blank" rel="noopener noreferrer" className="block break-all text-teal-700">자료 열기: {row.url}</a>
    <a href={row.source_url} target="_blank" rel="noopener noreferrer" className="block break-all text-teal-700">공식 근거 공고 확인</a>
    {row.check_status&&<p className={row.check_status==='ok'?'text-gray-600':'text-amber-800'}>자동 확인 상태: {checkName[row.check_status]??row.check_status}{row.consecutive_failures?` · 연속 실패 ${row.consecutive_failures}회`:''}</p>}
    {row.check_error&&<p className="text-amber-800">{errorText(row.check_error)}</p>}
    {row.content_hash&&<p className="break-all text-xs text-gray-500">버전 SHA-256: {row.content_hash}</p>}
    <p className="text-xs text-gray-500">자동 확인: {row.checked_at??'미확인'} · 검증 만료: {row.valid_until??'없음'}</p>
    <label className="block"><input type="checkbox" checked={same} onChange={e=>setSame(e.target.checked)}/> 같은 기관·사업임을 공식 공고와 대조했습니다.</label>
    <label className="block"><input type="checkbox" checked={intake} onChange={e=>setIntake(e.target.checked)}/> 현재 모집 연도·회차와 일치합니다.</label>
    <label className="block"><input type="checkbox" checked={destination} onChange={e=>setDestination(e.target.checked)}/> 실제 신청 화면·로그인 복귀 경로 또는 해당 제출 양식을 확인했습니다.</label>
    <label className="block">접근 방식<select value={access} onChange={e=>setAccess(e.target.value)} className={control}><option value="direct">직접 열림</option><option value="login_required">기관 로그인 필요</option></select></label>
    <label className="block">확인 근거<textarea value={note} onChange={e=>setNote(e.target.value)} minLength={20} maxLength={2000} className={control} placeholder="확인한 사업·모집 회차·화면을 20자 이상 기록하세요."/></label>
    <label className="block">유효 시간 (최대 7일)<input type="number" value={hours} min={1} max={168} onChange={e=>setHours(Number(e.target.value))} className={control}/></label>
    <div className="flex gap-3"><button type="button" disabled={busy||!same||!intake||!destination||note.trim().length<20} onClick={()=>void review('verified')} className="rounded bg-teal-700 px-3 py-2 text-white disabled:opacity-50">확인한 자료 승인</button><button type="button" disabled={busy||note.trim().length<20} onClick={()=>void review('rejected')} className="rounded border px-3 py-2 disabled:opacity-50">제외</button></div>
    {row.kind==='template'&&!row.file_format&&<p>양식 승인은 수집기가 최근 24시간 안에 파일 내용·형식·크기를 확인한 뒤 가능합니다. 현재 PDF·HWP를 검사하며 HWPX·DOCX는 자동 승인하지 않습니다.</p>}
    <p className="text-xs text-gray-500">승인 가능한 주소: 수집된 공고 공식 주소와 같은 기관 도메인이거나 승인된 출처 도메인. 관리자가 입력한 공식 주소, 블로그·폼 같은 공유 호스팅은 승인 도메인 등록이 필요합니다.</p>
    {row.kind==='template'&&row.file_format==='pdf'&&!!row.content_hash&&<div className="border-t pt-2">
      <button type="button" onClick={()=>setDesigning(d=>!d)} className="rounded border px-3 py-2">{designing?'입력 칸 설계 닫기':'서류 작성 도우미 입력 칸 설계'}</button>
      {designing&&<div className="mt-2"><FormTemplateDesigner resourceId={row.id}/></div>}
    </div>}
    {!!error&&<p role="alert" className="text-red-700">{error}</p>}
  </article>;
}
