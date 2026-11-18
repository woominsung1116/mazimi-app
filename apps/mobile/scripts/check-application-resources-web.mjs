// Mock-only browser replay: never contacts an institution or production API.
// node scripts/check-application-resources-web.mjs EXPORT_DIR [ADMIN_WEB_ORIGIN]
// Browser and server come from web-check-kit.mjs.
import assert from 'node:assert/strict';
import {serveExport,launchBrowser} from './web-check-kit.mjs';
const {origin,close}=await serveExport(process.argv[2]);
// Programme and file ids go into API paths, so the app takes UUIDs only (app/form-fill.tsx, security review L4).
const PROGRAM='aaaaaaaa-1111-4111-8111-111111111111', FILE='bbbbbbbb-2222-4222-8222-222222222222', APPLY='cccccccc-3333-4333-8333-333333333333';
let browser;
try {
  browser=await launchBrowser();
  const context=await browser.newContext({viewport:{width:390,height:844}});
  await context.addInitScript(()=>{
    sessionStorage.setItem('mazimi_auth_token','fixture-only-token');
    localStorage.setItem('mazimi_consent_v1',JSON.stringify({required:{termsAgreed:true,privacyAgreed:true,agreedAt:'2026-10-07',termsVersion:'2026-03-21',privacyVersion:'2026-09-26'},healthInsurance:{agreed:false}}));
  });
  const notice={url:'https://official.example.org/notice',origin:'https://official.example.org',kind:'information',label:'공고 확인',explanation:'방문·이메일 신청은 공고에서 확인하세요.'};
  const template={id:FILE,kind:'template',url:'https://official.example.org/download?file=1',source_url:notice.url,label:'신청서.hwp',file_format:'hwp',byte_size:41472,content_hash:'hash1',access:'direct',verified_at:'2026-10-07T00:00:00Z',valid_until:'2099-01-01T00:00:00Z'};
  const destination={url:'https://form.example.org/apply?intake=2026',origin:'https://form.example.org',kind:'login_required',label:'로그인 후 신청하기',explanation:'기관 로그인 후 이어가세요.'};
  const resources={notice:null,application:{...template,id:APPLY,kind:'application',url:destination.url,access:'login_required',file_format:null},templates:[template],status:'verified'};
  const policy={mode:'assisted',can_prepare:true,can_autofill:false,can_submit:false,reason:'기관에서 최종 제출',official_origin:destination.origin,destination,notice_destination:notice,resources,
    workflow:{version:1,channels:[{id:'online',label:'온라인 접수',evidence:'온라인신청'},{id:'visit',label:'방문 접수',evidence:'방문신청'}],no_application_required:false,needs_confirmation:false,method_text:'온라인신청||방문신청'}};
  const plan={revision:'v1',title:'2026년 양식 검증 테스트',policy,materials:{documents:[],document_text:null,links:[],emails:[]},tasks:[{id:'eligibility',kind:'eligibility',title:'조건 확인',detail:'공고 확인'}],completed_tasks:[],progress_reset:false,selected_channel:'online',official_links:[]};
  let stale=false;const opened=[];const errors=[];
  await context.route('**/*',async route=>{
    const request=route.request(),url=new URL(request.url());let body={items:[],total:0};
    if(url.pathname.startsWith('/api/v1/')) {
      if(url.pathname.endsWith('/auth/me'))body={id:'fixture-user',role:'user'};
      else if(url.pathname.endsWith('/profile'))body={profile:{birth_year:2001,region_code:'busan'}};
      else if(url.pathname.endsWith('/preparation')){if(request.method()==='PUT'){Object.assign(plan,request.postDataJSON());}body=plan;}
      else if(url.pathname.endsWith('/submission'))body={connector:null,submission:null};
      else if(url.pathname.endsWith('/handoff'))body={journey:null};
      else if(url.pathname.endsWith(`/programs/${PROGRAM}`))body={id:PROGRAM,title:plan.title,application_policy:{...policy,resources:{...resources,templates:stale?[]:[template]}}};
      await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(body)});return;
    }
    if(['official.example.org','form.example.org'].includes(url.hostname)){opened.push(request.url());await route.fulfill({contentType:'text/html',body:'<h1>Mock official destination</h1>'});return;}
    if(url.origin===origin){await route.continue();return;}
    await route.abort();
  });
  const page=await context.newPage();page.on('pageerror',error=>errors.push(error.message));
  await page.goto(`${origin}/apply-assistant?programId=${PROGRAM}`);
  await page.getByText('공식 신청처 · 제출 양식',{exact:true}).waitFor();
  await page.getByRole('button',{name:'로그인 후 신청하기',exact:true}).waitFor();
  await page.getByRole('radio',{name:'방문 접수',exact:true}).click();
  await page.getByRole('button',{name:'준비 저장하고 방문 접수 안내 열기',exact:true}).waitFor();
  assert.equal(await page.getByRole('button',{name:'로그인 후 신청하기',exact:true}).count(),0);
  const popup=page.waitForEvent('popup');
  await page.getByRole('button',{name:'공식 양식 받기 신청서.hwp'}).click();
  const file=await popup;await file.waitForURL(template.url);await file.close();
  assert.equal(opened.length,1);
  // The web keeps no files in the vault (lib/vault.ts vaultFilesSupported), so 서류 보관함에 넣기 is not offered.
  assert.equal(await page.getByRole('button',{name:'서류 보관함에 넣기 신청서.hwp'}).count(),0,'no vault action on web');
  assert.equal(await page.getByText('서류 보관함에 넣기').count(),0);
  // 서류 작성 도우미 entry: the web never fills a form (an HWP is filled on device only, from Mazimi's
  // converted PDF; app/form-fill.tsx), so the browser shows the guided copy sheet.
  await page.getByRole('button',{name:'서류 작성 도우미 신청서.hwp'}).click();
  await page.getByText('웹에서는 양식을 직접 작성할 수 없어요. 앱에서 이용하거나 아래 내용을 원본 양식에 옮겨 적어 주세요.',{exact:true}).waitFor();
  await page.getByText('프로필에는 출생 연도(2001년)만 있어요. 월·일을 포함해 입력해 주세요.',{exact:true}).waitFor();
  assert.ok(await page.getByText('작성할 때만 입력해요. 저장하지 않고, 파일을 만든 뒤 바로 지워요.').count()>=2,'RRN and account are never prefilled');
  const original=page.waitForEvent('popup');
  await page.getByRole('button',{name:'원본 양식 열기',exact:true}).click();
  const sheet=await original;await sheet.waitForURL(template.url);await sheet.close();
  assert.equal(opened.length,2);
  await page.goBack();
  await page.getByText('공식 신청처 · 제출 양식',{exact:true}).waitFor();
  // Reuse the rendered card after the server invalidates its template.
  stale=true;
    await page.getByRole('button',{name:'공식 양식 받기 신청서.hwp'}).click();
  await page.getByText('자료가 변경됐거나 확인 기한이 지났어요. 새 공고를 확인해 주세요.').waitFor();
  assert.equal(opened.length,2);
  assert.deepEqual(errors,[]);
  await context.close();
  const checks=['login_required_cta','offline_channel_cta','official_template_open','vault_action_hidden_on_web','form_fill_guided_sheet','form_fill_open_original','stale_template_blocked','no_runtime_errors','external_network_blocked'];
  if(process.argv[3]) {
    const adminOrigin=process.argv[3];const admin=await browser.newContext();
    await admin.addInitScript(()=>localStorage.setItem('wello_token','fixture-admin'));
    let reviews=0;
    await admin.route('**/*',async route=>{
      const url=new URL(route.request().url());
      if(url.pathname.startsWith('/api/v1/')) {
        let body={};let status=200;
        if(url.pathname.endsWith('/auth/me'))body={id:'admin',role:'admin'};
        else if(url.pathname.includes('/application-resources/')&&route.request().method()==='PUT'){reviews++;status=409;body={error:'changed',code:'resource_changed'};}
        else if(url.pathname.endsWith('/application-resources'))body={program_title:plan.title,program_fingerprint:'fp1',items:[{...template,id:'resource1',kind:'application',status:'candidate',revision:2,program_fingerprint:'fp1',checked_at:null,check_error:null}]};
        else body={id:PROGRAM,title:plan.title,program_type:'policy',is_active:true};
        await route.fulfill({status,contentType:'application/json',body:JSON.stringify(body)});return;
      }
      if(url.origin===adminOrigin)await route.continue();else await route.abort();
    });
    const adminPage=await admin.newPage();await adminPage.goto(`${adminOrigin}/admin/programs/${PROGRAM}`);
    const approve=adminPage.getByRole('button',{name:'확인한 자료 승인'});await approve.waitFor();assert.equal(await approve.isDisabled(),true);
    for(const checkbox of await adminPage.locator('article input[type="checkbox"]').all())await checkbox.check();
    await adminPage.getByPlaceholder('확인한 사업·모집 회차·화면을 20자 이상 기록하세요.').fill('동일 기관과 현재 모집 회차의 신청 경로를 확인한 브라우저 테스트입니다.');
    await approve.click();
    await adminPage.getByText('공고·자료 또는 검증 상태가 변경됐습니다. 목록을 새로고침한 뒤 다시 확인하세요.').waitFor();
    assert.equal(reviews,1);checks.push('admin_review_confirmation_gate','admin_conflict_message');await admin.close();
  }
  console.log(JSON.stringify({status:'passed',checks,production_writes:0}));
}finally{await browser?.close();await close();}
