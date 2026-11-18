// Dependency-free browser smoke for an Expo web export. All API calls are mocked;
// no real account, production API, model, or notification service is contacted.
// node scripts/check-assistant-web.mjs EXPORT_DIR
// Browser, server and readiness come from web-check-kit.mjs (load event, then the screen's own text).
import assert from 'node:assert/strict';
import { mkdtemp } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { serveExport, launchBrowser, gotoApp, reloadApp, waitText as waitFor, clickText, clickLabel as clickFor } from './web-check-kit.mjs';

const output = await mkdtemp(join(tmpdir(), 'mazimi-assistant-ui-'));
const { origin, close } = await serveExport(process.argv[2] ?? '/tmp/mazimi-assistant-web-20261007');
let browser;
try {
  browser = await launchBrowser();
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 1, isMobile: true });
  const page = await context.newPage(); const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const evaluate = expression => page.evaluate(expression);
  const waitText = text => waitFor(page, text);
  const click = (text, role) => clickText(page, text, role);
  const clickLabel = (label, role) => clickFor(page, label, role);
  const fixture = {
    version: 1, program_id: 'fixture-program', title: '테스트 장학금', mode: 'rules', ai_available: true,
    amounts: { monthly: null, semester: 300000, once: null },
    notice: '현재 저장된 프로필 기준이며 질문만으로 설정을 변경하지 않아요.',
    eligibility: { status: 'unknown', summary: '아직 확인할 조건이 있어요.', checks: [{ label: '필수 조건', status: 'unknown', detail: '소득 구간을 확인해 주세요.' }], missing_fields: ['income_bracket'], profile_version: 1 },
    preparation: { revision: 'fixture-revision', title: '테스트 장학금', tasks: [{ id: 'eligibility', title: '신청 조건 확인', detail: '공식 조건 확인', kind: 'eligibility' }, { id: 'document:재학증명서', title: '재학증명서', detail: '발급 형식 확인', kind: 'document' }], completed_tasks: [], progress_reset: false, selected_channel: 'online', materials: { documents: [], document_text: null, links: [], emails: [] }, policy: { can_prepare: true, workflow: { no_application_required: false, channels: [] }, destination: null } },
    remaining_task_ids: ['eligibility', 'document:재학증명서'], alert_enabled: false, deadline_at: null,
    source: { official_url: 'https://example.org/benefit', last_synced_at: null, program_updated_at: '2026-10-07T00:00:00Z', checked_at: '2026-10-07T00:00:00Z', note: '저장 자료이며 지금 원문을 재검증했다는 뜻은 아니에요.' },
    sections: [{ topic: 'summary', title: '혜택 한눈에 보기', body: '저장된 공식 자료 요약', basis: 'program_record' }],
  };
  await context.addInitScript({ content: `
    if(!sessionStorage.getItem('__skipConsentFixture')) localStorage.setItem('mazimi_consent_v1', JSON.stringify({required:{termsAgreed:true,privacyAgreed:true,agreedAt:'2026-10-07',termsVersion:'2026-03-21',privacyVersion:'2026-09-26'},healthInsurance:{agreed:false}}));
    sessionStorage.setItem('mazimi_auth_token','fixture-only-token');
    window.__writes=0; window.__questions=[]; window.__comparisons=[]; window.__failQuestion=false;
    const data=${JSON.stringify(fixture)};
    const candidates=[{...data,program_id:'11111111-1111-4111-8111-111111111111',title:'복학 장학금'}, {...data,program_id:'22222222-2222-4222-8222-222222222222',title:'주거 지원'}];
    const nativeFetch=window.fetch.bind(window);
    window.fetch=async (url, options={})=>{
      const path=String(url);
      if (!path.includes('/api/v1/')) { if(new URL(path,location.href).origin!==location.origin) throw new Error('External network disabled in smoke test'); return nativeFetch(url, options); }
      let body={items:[],total:0,unread:0};
      if(path.endsWith('/auth/me')) body={id:'fixture-user',nickname:'테스트 사용자',role:'user'};
      if(path.includes('/assistant/bundles?')) body={version:1,scenario:'returning_student',profile_version:1,required_profile_fields:[],groups:[{title:'학비·학교생활',basis:'published_type_or_title_keyword',total_candidates:2,items:candidates.map(p=>({program_id:p.program_id,title:p.title,program_type:'scholarship',eligibility_status:'unknown',deadline:null,reasons:['저장한 조건으로 찾은 후보'],missing_checks:[]}))}]};
      if(path.endsWith('/assistant/compare')) {
        const request=JSON.parse(options.body);window.__comparisons.push(request);
        if(sessionStorage.getItem('__expiredComparison')) return new Response(JSON.stringify({error:'선택한 혜택이 만료되었어요.'}),{status:404,headers:{'Content-Type':'application/json'}});
        body={version:1,programs:request.program_ids.map(id=>candidates.find(p=>p.program_id===id)),amounts_additive:false,pairs:[],compatibility_notice:'중복 수혜 근거가 없어 확인이 필요해요.',documents_notice:'이름이 같아도 발급일과 제출 형식은 각각 확인하세요.',shared_documents:[{name:'재학증명서',uses:candidates.map((p,i)=>({program_id:p.program_id,document_id:'doc',required:i===0,evidence:i===0?'최근 30일 이내 원본':'사본 제출 가능',source:'program_documents'}))}]};
      }
      if(path.endsWith('/fixture-program/assistant')) {
        if(options.method==='POST') { if(window.__failQuestion) throw new Error('Fixture network failure'); const input=JSON.parse(options.body); window.__questions.push(input); data.mode=input.allow_ai_processing?'fallback':'rules';
          // A question is answered with the notice's own sentences (GroundedAnswer, lib/assistantTypes.ts), never generated text.
          body={...data,answer:{topics:['documents'],follow_up_topic:'documents',passages:[{id:'p-documents',text:'테스트 재학증명서 발급일을 확인하세요.',highlights:[],section:'documents',kind:'official_text',source_label:'공고',captured_at:null}],not_found:false,unsupported:false,notice:'공고 문장을 그대로 보여 드려요.'}};
          return new Response(JSON.stringify(body),{status:200,headers:{'Content-Type':'application/json'}}); }
        body=data;
      }
      if(path.endsWith('/alerts/preferences')&&options.method==='POST') {window.__writes++;data.alert_enabled=JSON.parse(options.body).enabled;body={enabled:data.alert_enabled};}
      return new Response(JSON.stringify(body),{status:200,headers:{'Content-Type':'application/json'}});
    };
  ` });
  await gotoApp(page, `${origin}/benefit-assistant?programId=fixture-program`);
  await waitText('테스트 장학금'); await waitText('소득 구간');
  assert.equal(await evaluate('document.body.innerText.includes("benefit-assistant")'), false, 'internal route name leaked through an unregistered Stack screen');
  assert.equal(await evaluate('window.__writes'), 0);
  await click('필요한 서류는?'); await waitText('테스트 재학증명서 발급일을 확인하세요.');
  assert.deepEqual(await evaluate('window.__questions'), [{ question: '필요한 서류는?', allow_ai_processing: false }]);
  assert.equal(await evaluate('window.__writes'), 0);
  await click('이 혜택 알림 켜기'); await waitText('이 혜택 알림 끄기');
  assert.equal(await evaluate('window.__writes'), 1);
  // After the first answer the screen offers that topic's follow-ups (lib/grounding.ts) in place of the starter chips.
  await evaluate('window.__failQuestion=true'); await click('어떻게 신청해?');
  // A failed request reads as a connection problem (lib/errorCopy.ts); the raw error never reaches the screen.
  await waitText('연결을 확인한 뒤 다시 시도해 주세요.'); assert.equal(await evaluate('document.body.innerText.includes("Fixture network failure")'), false, 'raw error text shown');
  assert.equal(await evaluate('window.__writes'), 1);
  assert.deepEqual(errors, []);
  await evaluate(`document.querySelector('textarea').scrollIntoView({block:'center'})`);
  await page.screenshot({ path: join(output, 'assistant.png') });
  await gotoApp(page, `${origin}/assistant`);
  await waitText('복학 장학금');
  assert.equal(await evaluate('document.body.innerText.includes("assistant")'), false);
  // Each candidate's checkbox is named for its programme (app/assistant.tsx); 비교에 담기 turns into 비교에 담음.
  await clickLabel('복학 장학금 비교 선택', 'checkbox'); await clickLabel('주거 지원 비교 선택', 'checkbox');
  await waitText('혜택 비교 · 2/4개 선택'); await click('선택한 혜택 비교하기');
  await waitText('함께 준비할 서류 후보');
  // A shared document is listed once per programme with that programme's own requirement. The source
  // evidence strings are pipeline metadata, kept off member screens since 9ddb24df.
  await waitText('복학 장학금 · 필수'); await waitText('주거 지원 · 선택');
  assert.equal(await evaluate('/최근 30일 이내 원본|사본 제출 가능/.test(document.body.innerText)'), false, 'source evidence shown to a member');
  assert.equal(await evaluate('document.body.innerText.includes("stack-calculator")'), false);
  assert.equal(await evaluate('window.__writes'), 0);
  assert.deepEqual(await evaluate('window.__comparisons[0]'), {program_ids:['11111111-1111-4111-8111-111111111111','22222222-2222-4222-8222-222222222222']});
  assert.equal(await evaluate('/6%|총 예상 수혜액|연간 지원금 예상 합계/.test(document.body.innerText)'),false);
  await page.setViewportSize({ width: 320, height: 740 });
  assert.equal(await evaluate('document.documentElement.scrollWidth <= 320'),true,'comparison overflows a narrow viewport');
  await evaluate(`document.querySelector('[role="link"]').scrollIntoView({block:'center'})`);
  await page.screenshot({ path: join(output, 'comparison.png') });
  await evaluate(`sessionStorage.setItem('__expiredComparison','true')`);
  await reloadApp(page); await waitText('현재 비교할 수 없는 혜택이 있어요');
  assert.equal(await evaluate('document.body.innerText.includes("공고 등록 금액")'),false,'stale comparison survived an expired selection');
  await click('혜택 다시 고르기'); await waitText('복학 장학금');
  await evaluate(`sessionStorage.setItem('__skipConsentFixture','true');localStorage.removeItem('mazimi_consent_v1')`);
  await reloadApp(page); await waitText('약관 동의');
  assert.equal(await evaluate('document.body.innerText.includes("복학 장학금")'), false, 'direct assistant route bypassed the required consent gate');
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ passed: true, checks: ['registered routes', 'profile gap', 'read-only question', 'explicit alert write', 'network error recovery', 'scenario selection', 'read-only comparison', 'separate document evidence', 'narrow viewport', 'expired comparison recovery', 'required consent gate'], screenshots: [join(output, 'assistant.png'),join(output,'comparison.png')] }));
} finally {
  await browser?.close(); await close();
}
