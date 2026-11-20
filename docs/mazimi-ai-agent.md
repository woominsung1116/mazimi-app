# 마지미 AI 혜택 비서 에이전트 계약

> 상태: 코드·테스트 완료, 운영 서버에서 켜져 있다(`ASSISTANT_AGENT_ENABLED=true`, 출시 전). 운영 계획 모델은 미세조정 플래너 `mazimi-planner-q17-v2`(`ASSISTANT_AGENT_PLANNER_PROMPT=tuned`, 계획 타임아웃 12초)이고 qwen3.5:9b는 쓰지 않는다. API는 `compose.yml`과 `infra/docker/compose.planner.yml`을 함께 넣어 올린다. 덧씌움 파일을 빼면 API가 `planner` 네트워크에서 빠지고 플래너 워밍업이 매시간 실패해 가드·분류기·규칙만 답한다(2026-11-12에서 2026-11-18까지 이 상태였다).
> 기준 문서: [제품 기획서](mazimi-ai-assistant-product-plan.md) §4·§5·§7·§10·§11·§13, [기술 스펙](mazimi-ai-assistant-technical-spec.md) §3~§15. 질문 1건 Q&A·생성 요약은 [v1/v2 계약](mazimi-ai-assistant-v1-contract.md)이 그대로 기준이다.
> 이 문서가 에이전트의 엔드포인트·수치·코드 위치의 기준이다. 스펙과 다른 결정은 §12에 모았다.

## 1. 한 줄 정의

사용자의 목표 하나를 `PLAN → EXECUTE → CONFIRM → RESULT`로 처리한다. 읽기 도구는 바로 실행하고, 상태를 바꾸는 도구는 **확인 카드**만 만든다. 사용자가 그 카드의 해시를 승인해야 서버가 한 번 실행한다. 판정·금액·날짜·서류는 기존 서비스가 정하고 모델은 정하지 않는다.

## 2. 실행 흐름

1. `POST /assistant/conversations/{id}/messages {goal}` → 202 `{run_id}`. 목표는 저장 전에 `redact_question`(이메일·URL·`www.`·6자리 이상 숫자열, 그리고 `비번`·`아이디`·`pw:` 같은 인증 라벨 뒤 두 단어 안의 비밀번호 모양 값)으로 가리고 300자로 자른다. 이후 모든 단계는 가린 문장만 쓴다. 인증 라벨 뒤를 가린 목표는 거절한다(`Refusal::Credential`). 검색 키워드는 라틴 문자와 숫자·기호가 섞인 말(`hunter2`)을 저장하지 않는다.
2. **PLAN**
   - 거절 규칙 먼저(`agent/goals.rs`): 명시적 지시 무시(`이전 지시 … 무시`)·관리자 비밀·가정·합산/금리 계산은 모양과 상관없이 거절한다. 다른 사용자 정보·대리 신청/결제·프로필 변경·혜택 동시 수혜는 명령일 때만 거절하고, 질문이면 보류한다(모델, 꺼져 있으면 칩). `sql`·`토큰`·`프롬프트`·`해킹`·`api 키`·목적어 있는 `무시하고`(`소득 기준 무시하고`)는 거절 단서가 아니라 쓰기 차단 표지다. 대리 신청이나 `대신`이 든 상태 보고는 상태 기록이 아니라 대리 신청 거절이다. 거절된 목표는 모델에 가지 않는다.
   - **라우팅 가드**(`agent/guard.rs`, 규칙 라우팅 전에 돈다):
     - 입력 정규화(`rules::normalize`, 모든 단서 비교 전): NFKC(전각 문자, 붙여 쓴 자모 → 음절), 폭 없는 문자·한글 채움 문자 제거, 남은 첫소리를 뒤에 음절이 올 때만 앞 음절 받침으로 닫기(`ㅅㅣㅅㅡㅌㅔㅁ상` → `시스템상`), 키릴·그리스 닮은꼴 → 라틴, 비ASCII 괄호 → `[ ]`, `s y s t e m`처럼 띄어 쓴 라틴 단어 붙이기. 표지 검사(쓰기 차단·주입 거절)는 여기에 더해 낱말 끝 첫소리도 닫고 띄어 쓴 자모도 붙인 두 번째 형태(`rules::normalize_marker`, `ㅅㅣㅅㅡㅌㅔㅁ` → `시스템`)로도 본다. 라우팅은 이 형태를 쓰지 않는다(`켜줘ㅋㅋ`). 일본어·중국어 표기(`システム`)는 다루지 않는다.
     - 거절(강한 표지만): 인증 라벨 뒤를 가린 목표, 닫힌 역할 태그(`[SYSTEM:`·`<admin>`·`admin:`)와 템플릿·코드 토큰, SQL 문(`select … from`, SQL 단어 옆 `;`), 명시적 무시·망각(지시어 `지시사항`·`명령어`·`지침서` 포함, `이전 대화 내용 무시`)·권한·모드 주장·DB 요구, 명령 모양의 역할 바꾸기·내부 지시 요구(질문이면 보류), 대리 신청·로그인 명령(좁은 꼴만: `나/내/제/저 대신`, `로그인해서`, `알아서` 뒤에 제출 동사(`신청`·`접수`·`제출`·`응모`·`넣`)와 그 명령이 목표 끝에 올 때. `해 줘`·`좀 해주세요`·`해줄래`·`해주실래요`처럼 띄어 써도 같다. 질문·방법 요청·`X 대신 Y`("대신" = instead)·애매한 꼴은 그대로 라우팅한다. 제출 도구가 없어서 놓쳐도 아무것도 제안되지 않는다), 질문이 아닌 다른 사용자 데이터 요청(계정 명사 바로 앞의 제3자·소유격·`계정`, 전체 사용자, `user 42`·`user_id` 같은 번호 계정). `친구 추천 이벤트 알림`처럼 주인이 없는 제3자는 보류다. 거절 모양은 기존 규칙 거절과 같다(`Decision::Refuse`).
     - **확인 생략 요청은 거절도 보류도 쓰기 차단도 하지 않는다**(라우팅 v4, 사용자 결정). `확인 없이`·`컨펌 안 받고`·`묻지 말고`·`그냥 다 …해` 같은 목표도 그대로 라우팅하고, 쓰기는 다른 목표처럼 확인 카드가 된다. 그런 목표의 답에는 고정 문장 "저장·알림 같은 변경은 꼭 확인을 거쳐야 해요."를 붙인다(`guard::mentions_bypass`, `run::with_bypass_notice`). 패턴으로 거절하던 방식은 세 번의 리뷰에서 양쪽으로 계속 샜다.
     - 보류(모델로, 모델이 꺼져 있으면 칩): 약한 거절 단서(대리 신청 단서·프로필 변경 요청, 금리·가격 순위, 질문 모양의 다른 사용자 데이터), 행동 `-고` 뒤의 두 번째 요청, 목록·대화 참조(`두 번째`, `아까 본`), 요청 부정(`켜지 마`, `A 말고`, `빼고`, `필요 없어`), 기록할 수 없는 결과(불합격·탈락), 시각이 붙은 알림, 서류 이름 없는 체크(상태가 있으면 상태 기록으로), 상태 두 개, 혜택 단어 없는 검색·훑기(`부산 날씨`). `청년`·`대학생` 같은 대상 단어는 혜택 단어로 친다.
     - 쓰기 차단: 약한 표지(괄호·꺾쇠·인용 부호 같은 Ps/Pe/Pi/Pf 문자(ASCII 괄호와 휴대폰 둥근 따옴표 제외)·`시스템`·`지시`·`규칙`·가린 내용·닮은꼴 문자(그리스·키릴·IPA·작은 대문자·체로키) 등)라도 있으면 규칙은 쓰기 계획을 내지 않고, 모델 계획도 쓰기 단계를 버린다(쓰기만 있으면 거절, `planner::interpret`).
   - **의도 분류기**(`agent/classifier/`, `ASSISTANT_INTENT_CLASSIFIER_ENABLED`, 기본 꺼짐): 켜져 있고 산출물 검증이 끝났으면 순서는 가드(`goals::guarded`: 본인 프로필 카드, 거절, 대리 상태 보고 거절) → **분류기의 확신 거절** → 찾은 혜택 후속(`found_decision`)·거절한 부분(`declined_decision`)·v3 도구 모양 → **규칙의 확신 모양**(`goals::decide_with_model_and`) → 분류기의 답 → (0.7 미만) 모델(AI 켜짐) 또는 규칙(AI 꺼짐)이다. 결정적인 관례(신청 준비 여정, 카탈로그 마감 임박 검색, 찾은 혜택 후속)는 규칙이 답하고, 분류기·모델은 규칙이 못 읽는 의역·오타·간접 표현을 맡는다(핸드오프 `router-tuning.md`).
     - 거절 라벨은 `refuse`와 `refuse_fraud`(소득·재산·근로·거주를 사실과 다르게 신고하는 방법. 답은 고정 문구 `run::MISREPORT_REFUSAL`) 둘이고, 두 확률의 합이 산출물의 거절 기준(`refuse_threshold`, 0.99) 이상일 때만 답이다. 규칙 쪽 부정수급 거절은 지워졌고 이 부류는 모델 쪽만 맡으므로 정밀도가 우선이다(정직한 질문 "위장전입으로 받은 돈 반환" 등에서 분류기의 잘못된 거절 0). 기준 미만이지만 1위가 거절인 목표(의심 목표)는 평소대로 라우팅하되 **쓰기 단계를 계획하지 않는다**(`run::plan_route`가 규칙 전에 실행 맥락의 `suspect`를 세우고, 의도 층이 쓰기를 남기지도 만들지도 않는다): 부정 신고 + 쓰기 요청은 카드가 생기지 않고, 같은 단어를 쓴 정직한 질문은 읽기 답을 받는다. 쓰기만 남으면 대체 안내다. 확인 생략 문구의 거절은 답이 아니다(라우팅 v4). **분류기는 기본 꺼짐이므로 운영에서는 부정수급(허위 신고) 거절이 없다**: 분류기가 꺼져 있으면 부정 신고 문구도 평소대로 라우팅되고(검색·읽기 답), 쓰기는 여전히 사용자가 확인해야 하는 카드뿐이다. 목표 전체 거절은 다음 단계로, 쓰기 이해 모델에 부정 신고 행위를 추가해 맡긴다(`router-tuning.md`의 계획).
     - **주제 헤드**(산출물 형식 /2): e5 임베딩 위 로지스틱 회귀가 검색 목표의 분야(주거·취업·교육·금융·생활·복지 또는 `none`)를 고른다. 어휘(`categories::fields_named`)가 분야를 하나도 못 읽은 검색에만, 1위가 2위보다 0.3 이상 높을 때 채운다(`classifier::with_topic`, `run::plan`이 규칙·분류기·모델 계획 모두에 적용). "면접 볼 때 돈 주는 제도" → 취업.
     - 목표의 문자 1–4그램 TF-IDF와 multilingual-e5-small 문장 임베딩(`query: ` 접두, 평균 풀링, 표준화 후 ×0.15)을 이어 붙이고, 다항 로지스틱 회귀가 라벨(도구 순서들, `refuse`, `refuse_fraud`) 중 하나를 고른다. 두 거절 라벨은 확률을 합쳐 0.99 기준선으로 본다. 화면 혜택 자격 확인 단독과 확인+공고 인용은 같은 라벨이다.
     - 신뢰도가 0.7(산출물 값. `ASSISTANT_INTENT_CLASSIFIER_THRESHOLD`, 0.5–0.99) 이상이면 라벨을 모델 응답 모양으로 바꿔 모델 계획과 같은 `planner::interpret`에 넣는다. 분류기는 읽기와 양식·공식 페이지 안내만 계획한다(알림·저장·상태·체크 쓰기는 의도 층의 몫이고, 쓰기만인 라벨은 규칙이나 모델 플래너로 넘긴다). 프로그램 토큰만 채운다. 그래서 인자 채우기·`needs_program`·인젝션 표지 쓰기 차단·검증은 모델 경로와 같고, 쓰기는 확인 카드다.
     - 0.7 미만이거나 `interpret`가 응답을 거부하면: 모델이 켜져 있으면 먼저 `goals::decide_with_model`(규칙의 확신 모양)이 답하고, 그것도 없을 때만 모델(바쁘거나 실패하면 규칙 답, 없으면 칩), 꺼져 있으면 `goals::decide`(규칙 답, 없으면 칩). 로컬 fd 세트에서 규칙 먼저가 모델 직행보다 도구 0.907 대 0.852, 잘못된 거절 0 대 2, 모델 호출 13% 대 27%였다.
     - 확인 생략 문구(라우팅 v4 정책: 거절하지 않고 카드로 보낸다): 분류기의 `refuse`가 확인 생략 문구(`guard::mentions_bypass`)가 든 목표에 나오면 답으로 쓰지 않고 넘긴다. 진짜 거절은 분류기 앞의 가드가 한다. 학습 자료도 이 정책으로 고쳤다(확인 생략 문구를 거절로 둔 행은 요청한 도구로 바꾸거나, 가드만 있는 경로가 넘기는 행은 뺐다).
     - 저장: 계획과 혜택 고르기는 `planner = fast_path`, 분류기의 `refuse`는 `planner = refused`(모델의 거절과 같은 칩 안내). `goal_kind = classifier`, `provider = intent-classifier`, `model` = 산출물 버전, `prompt_version = label:<도구 순서>`. 로그에는 라벨·신뢰도·시간만 남고 목표는 남지 않는다.
     - 산출물이 없거나 sha256이 다르면 시작할 때 한 번 경고하고 꺼진 채로 돈다. 그때 라우팅은 분류기가 없을 때와 같다. 적재는 백그라운드 스레드에서 하고(약 1.5–2초), 끝날 때까지도 분류기 없이 돈다.
   - **빠른 경로**: v1 규칙 분류기와 같은 단어 단서 문법으로 흔한 목표를 고정 계획으로 바꾼다(마이크로초). 두 요청이 이어진 목표(`보고`, `맞으면`, `…까지`, `둘 다`)는 반쪽 계획을 막으려고 빠른 경로에서 뺀다.
     - 한글이 없는 목표(영어)와 `도와줘`만 있는 목표는 빠른 경로에서 뺀다(모델, 꺼져 있으면 칩).
     - 훑기(검색+마감)는 기간(`이번 학기`·`요즘`)이나 챙김 단어(`챙겨`·`놓친`)가 있을 때만이다. `받을 수 있는 X 있어?`는 검색이다.
     - 붙여 쓴 꼴을 읽는다: `저장해둔`·`찜해놓은`(저장한 혜택), `마감인거`, `찜해놔줘`, `체크 다시 풀어`, `준비 어디까지`.
     - 화면 혜택 없는 `이거 자격 돼?`는 혜택 고르기다.
     - 어휘(라우팅 v4): `뭐 떼야`·`발급받아야`·서류 + `준비해야` → `get_preparation`(지난 꼴 `뗐`·`발급받았`은 체크). 양식 명사(`신청서`·`지원서`·`자소서`…) 뒤 쓰기 동사(`써`·`쓰`·`채워`·`작성`, `썼` 제외) → `open_form_fill`. 요청 바로 앞의 알림 절(`마감 전에 (꼭) 알려줘`, `놓치지 않게 알려줘`, `결과 나오면 알려줘`) → `set_alert`(시각이 있으면 보류). `마감하는`도 마감 정리다.
   - **모델이 켜져 있을 때의 확신 규칙**(`goals::decide_with_model`, 라우팅 v4): 빠른 경로는 확신할 때만 답하고 나머지는 모델로 보낸다. 확신은 목표 단어가 도구 계열(알림·저장·양식·비교·마감 정리·준비·기록) **하나만** 가리키고, 아래가 아닐 때다.
     - 마감 정리에 지역·상태·유형이 붙음(`다음 달 마감 장학금`: 검색+마감일 수 있다).
     - 요청 모양의 공고 질문(`서류 목록 보여줘`: 공고인지 체크리스트인지), `랑`으로 이은 두 질문, 계열 이름 없는 마지막 갈래(공고 인용·검색·혜택 고르기)에서 계열이 하나라도 겹침.
     - `요즘`·`요새`만 있는 훑기(이 말은 기간이기도 하고 군말이기도 하다. 서버는 모델 검색에 이 말 때문에 마감 단계를 붙이지 않는다. 모델이 붙인 마감 단계는 둔다).
     - 질문 모양의 준비 목표(`신청하려면 뭐 준비해야 돼?`: 체크리스트인지 3단계 준비인지), `도와줘`·`챙겨줘`만 있는 준비, 선택이 2개 미만인데 `이 두 개 비교`.
     - 모델이 꺼져 있으면 `goals::decide`가 예전처럼 규칙으로 답한다. 모델이 켜져 있어도 바쁘거나 한도·장애·무효 출력이면 규칙 답(계획·혜택 고르기)이 있으면 그것을, 없으면 칩을 준다(`run::plan`, eval도 같다).
   - **인자**(`agent/args.rs`, 빠른 경로와 모델 경로가 같은 함수): 지역·재학 상태·유형은 목표 단어로만 정한다(영어 단어, `다니-` = 재학, `기업`·`학교`가 `할인`의 유형을 좁힘). 키워드는 목표에 있는 내용 명사만 남긴다(조사·동사 조각·대명사·`들`·틀 단어·기간·마감 단어·학교 이름·영어 기능어를 버리고, 라틴 문자는 대문자 약어만). 마감 기간은 목표가 마감을 말할 때만 읽는다(`이번 주` 7, `다음 주` 14, `N일`, `N주` 7N, `이번 달`·`다음 달`·`담달`·`한 달` 30, `오늘` 1, `next N days`, 최대 60). 띄어 쓰지 않은 말은 지역·혜택 명사·요청으로 나눈다(`부산장학금찾아줘` → `부산 장학금 찾아줘`. 학교 `부산대`는 지역이 아니다). 붙은 `지원`·`혜택`은 키워드에서 뗀다(`월세지원` → `월세`).
   - 화면 혜택이 필요한 목표인데 없으면 저장한 혜택 3개를 고르기 칩으로 준다.
   - 그 밖의 목표: 플래너(`ASSISTANT_AI_ENABLED` 또는 `ASSISTANT_AGENT_PLANNER_ENABLED`)가 있고 모델이 쉬고 있으면 **모델 1회 호출**로 JSON 계획을 받는다. 없으면 빠른 경로 목표 칩으로 안내한다. `ASSISTANT_MODELS_ENABLED=false`는 모든 모델 호출(답변 모델·플래너·분류기·재정렬·쓰기 이해)을 끄는 단일 스위치다. 따로 끄는 스위치: `ASSISTANT_AGENT_JOURNEY_ENABLED=false`(신청 준비 여정의 보관함 대조·초안 카드), `ASSISTANT_CARD_PRODUCER_ENABLED=false`(의도 층이 만든 쓰기 카드를 계획에 합치지 않음). 둘 다 기본 켜짐이고 compose가 넘긴다. 바쁘거나 실패하면 규칙 답이 있으면 그것을, 없으면 칩을 준다.
3. **EXECUTE**: 계획의 각 단계를 실행한다. 프로그램 토큰(`context`·`selected`·`saved`)을 사용자 데이터와 공개 조건으로 풀고(§4), 읽기 도구는 기존 서비스를 부른다(도구당 15초). 쓰기 도구는 현재 상태를 읽고 확인을 만든다. 이미 원하는 상태면 `skipped`로 끝난다.
4. **RESULT**: 도구 결과로 템플릿 메시지를 만든다(`agent/compose.rs`). 응답 검증기가 이 실행의 근거가 아닌 인용, 등록되지 않은 행동, 이 실행에 없는 프로그램 id를 지우고, 판정을 규칙 결과로 덮어쓴다.
5. **CONFIRM**: 확인이 남아 있으면 `awaiting_confirmation`. 승인·취소·만료가 끝나면 `completed`.

## 3. 상태 머신 (`mazimi_core::agent::RunStatus`)

```text
received → planning → tool_running → validating_response → completed
                    ↘ validating_response (거절·되묻기·대체 안내)
tool_running/validating_response → awaiting_confirmation ⇄ executing → completed
awaiting_confirmation → completed (모두 취소·만료)
비종료 상태 어디서든 → failed | timed_out | cancelled
```

- 전이는 `can_transition`이 유일한 정의다. DB 갱신은 `UPDATE … WHERE status = ANY(허용 출발 상태)` 한 문장이라 경쟁 상태에서도 불법 전이가 없다.
- 대화당 진행 중인 실행은 하나다(부분 유니크 인덱스). 두 번째 목표는 `409 run_in_progress`. 암묵 취소는 없다.
- 실행 전체 타임아웃 `ASSISTANT_AGENT_RUN_TIMEOUT_SECS`(기본 300초) → `timed_out`, 남은 확인은 만료.
- API 프로세스가 죽어 멈춘 실행은 워커 정리 작업이 1시간 뒤 `failed`(`abandoned`)로 닫는다.

## 4. 도구 (`agent-tools-v2`)

| 도구 | 권한 | 인자 | 재사용 서비스 | 결과(근거) |
|---|---|---|---|---|
| `search_benefits` | read | `region?`, `enrollment_status?`, `program_types?`, `keywords?`(≤3, 각 ≤12자, 문자·숫자만) | `benefit_assistant::search::search_with_intent` (추천 미리보기 + `filter_summary`) | 후보 ≤5, 자격 상태, 마감 (자격 check 인용 ≤2/건) |
| `check_eligibility` | read | `program` | `benefit_assistant::guidance` (구조 자격 + 하드 필터) | verdict·check별 상태·`missing_fields` (check 인용) |
| `answer_from_notice` | read | `program: context`, `question`(≤200) | `grounded_qa` (문장 색인·BM25·판정) | 인용 문장 ≤3 또는 not-found (문장 = 근거) |
| `get_preparation` | read | `program` | `application_preparation::load_on` | revision·항목·완료 여부·다음 항목 (신청 방법 원문 줄) |
| `compare` | read | `programs: selected\|saved` | `benefit_comparison::compare` | 2~4개 조건·마감·남은 준비·공통 서류. 중복 수혜 판단 없음 |
| `deadlines` | read | `within_days?`(1~60) | `benefit_assistant::briefing` | 저장·준비·알림 혜택의 마감 순 ≤3 |
| `open_form_fill` | confirm_write (v2) | `program` | 준비 계획의 검증된 양식 | 양식 목록을 읽고, 양식이 있으면 "기기에서 초안 채워 열기" 카드(양식 id·content_hash에 묶임). 승인은 서버 상태를 바꾸지 않고 앱 안 경로만 돌려준다. 양식이 없으면 읽기 결과(복사용 안내) |
| `bookmark` | confirm_write | `program`, `bookmarked` | `user_bookmarks` 삽입/삭제 | 확인 카드 |
| `set_alert` | confirm_write | `program`, `enabled` | 알림 구독 upsert(`/alerts/preferences`와 같은 SQL) | 확인 카드 |
| `toggle_checklist_item` | confirm_write | `program: context`, `task`(id 또는 제목 일부, ≤60), `done` | `application_preparation::save_on` | 확인 카드(항목을 하나로 못 고르면 제안 없음) |
| `record_application_status` | confirm_write | `program: context`, `status` | `routes::state::record_status_on` (이력 포함) | 확인 카드. 확정 상태는 뒤로 못 감(applied→planning 불가 등) |
| `profile_patch` (v2) | confirm_write | `field`(region_code·enrollment_status·employment_status·school_year·income_bracket·household_size), `value`(정규 코드·정수) | `profile_validation::normalize_profile` 후 해당 열만 UPDATE | 확인 카드(현재 값·`profile_version`에 묶임, 프로그램 id는 nil UUID). 값은 목표 문장에서 서버가 읽는다. 모델이 정하지 않는다 |
| `check_official_page` (v3) | read | `program` | 격리된 페치 서비스(MCP, DB 없음) → `mazimi_worker::sources::live_page` | 저장된 URL 중 승인 범위 안의 공식 페이지를 실시간으로 읽어 접수 상태(open·closed·upcoming·unknown)·기간·첨부 이름·우리 데이터와 다른 점, 원문 인용(≤4, 각 ≤160자)과 확인 시각. 실패는 `not_checked`("지금 공식 페이지를 확인하지 못했어요" + 링크) |
| `document_issue_link` (v3) | read | `document?`, `program?` | 코드 안 버전 카탈로그(`agent/documents.rs`) | 서류별 발급처·공식 링크(확인 날짜)·안내. 모르는 서류는 "발급처를 안내할 수 없어요" + 공고 링크 |
| `add_to_calendar` (v3) | confirm_write | `program`, `date`(deadline·application_start·announcement·interview), `days_before`(0–14, "마감 하루 전"). 둘 다 서버가 목표에서 읽음 | `agent/calendar.rs` | 확인 카드(날짜·근거 인용에 묶임). 승인은 서버 상태를 바꾸지 않고 기기가 expo-calendar로 일정을 쓴다. 날짜가 없거나 지났거나 여러 개면 카드 없음 |
| `match_vault_documents` (여정) | read | `program` | `agent/tools.rs`, 앱 `lib/agentVault.ts` | 필요 서류 목록(필수만, ≤30)과 체크리스트 완료 표시(`user_document_items`의 이름·상태)를 맞춘 have·needs_check·missing. 보관함 대조는 기기가 같은 목록으로 한다. 파일과 보관함 항목은 서버에 오지 않는다 |
| `request_signature` (여정) | confirm_write | `program` | `agent/run.rs` | 기기 서류 작성 도우미의 서명 단계로 넘기는 카드. 서버 상태를 바꾸지 않고 서명은 서버에 오지 않는다. 양식 초안이 없으면 건너뛴다. 모델은 계획하지 않는다 |
| `open_official_application` (여정) | confirm_write | `program` | `agent/run.rs`, `agent/compose.rs` | 검증된 공식 신청 페이지를 여는 카드. 아무것도 제출하지 않는다 |

- 계획 JSON: `{"steps":[{"tool":…,"args":{…}}]}`, 최대 6단계, 같은 단계 중복 금지. 모르는 도구·필드·값, UUID·URL·이메일 모양 문자열은 파싱 단계에서 거절(`Plan::parse`).
- 프로그램은 **토큰**으로만 가리킨다: `context`(화면의 혜택, 대화나 메시지의 `program_id`), `selected`(사용자가 고른 2~4개), `saved`(북마크·준비·알림 구독, 마감 순). 모델은 id를 만들 수 없다. 서버가 사용자 데이터와 공개 조건(`currently_available_sql`)으로 푼다. 상태 기록만 마감이 지난 공고도 허용한다.
- 토큰 확장 상한: 비교 4, 알림 5, 양식 1, 공식 페이지 2, 나머지 3.
- v3 도구 세 개는 각자 플래그가 있다(기본 off): `ASSISTANT_AGENT_OFFICIAL_PAGE_ENABLED`(+ `OFFICIAL_PAGE_MCP_URL`·`OFFICIAL_PAGE_MCP_TOKEN`), `ASSISTANT_AGENT_DOCUMENT_LINKS_ENABLED`, `ASSISTANT_AGENT_CALENDAR_ENABLED`. 꺼져 있으면 빠른 경로는 그 도구가 없던 때처럼 라우팅하고(`goals::decide_with`), 모델 계획에서는 그 단계를 버린다. 설계·보안 분석·배포 순서는 핸드오프 `agent-tools.md`.
- 칸에 값을 적어 달라는 목표(`이름 칸에 홍길동으로 적어줘`)는 값을 읽지 않고 `open_form_fill` 카드로 간다. 서버에서 채팅 값을 채우는 도구(`fill_form_fields`)는 사용자 결정으로 빠졌고, 채팅으로 말한 값은 앱이 기기 안에서 양식 칸에 맞춰 승인 카드로 넣는다(`apps/mobile/lib/formFill/chatFill.ts`). 그 값은 서버로 보내지 않는다.
- 결과 봉투(스펙 §7.3): `{seq, tool, permission, program_id?, ok, status, data, evidence[{id, program_id, kind, quote?, source_label, source_url?, captured_at?}], error?{code,message}, executed_at}`. 내부 오류는 일반 문구로만 나간다.

## 5. 확인과 실행 (스펙 §8)

1. 제안할 때 현재 상태를 읽어 **precondition**을 만든다: 북마크 `{bookmarked}`, 알림 `{enabled}`, 체크 `{revision, done}`, 상태 `{state}`.
2. `payload_hash = sha256(v, user, run, tool_call, tool, program, args, pre, exp)`(`mazimi_core::agent::confirmation_hash`, 키 정렬 JSON). 만료는 10분.
3. 카드에 변경 전/후, 실행할 행동, 되돌릴 수 있는지, 해시를 보여 준다.
4. `POST /assistant/confirmations/{id}/approve {payload_hash}` (`Idempotency-Key` 선택). 서버 순서:
   - 확인 행과 실행 행을 잠근다.
   - 이미 `executed`면 저장한 결과를 그대로 돌려준다(`replayed: true`).
   - `pending`이 아니면 409를 준다(`confirmation_expired`·`_rejected`·`stale_confirmation`).
   - 만료됐으면 409를 준다.
   - 보낸 해시가 저장 해시와 다르면 `409 payload_mismatch`(상수 시간 비교).
   - 저장 행으로 해시를 다시 계산해 다르면 실패로 처리한다.
   - 실행이 `awaiting_confirmation`이 아니면 409를 준다.
   - **현재 상태를 다시 읽어** precondition과 다르거나(공고 revision 변경, 그 사이 저장 등) 공고가 내려갔으면 `stale` + `409 stale_confirmation`.
   - 쓰기를 같은 트랜잭션의 세이브포인트에서 실행한다. 확인을 `executed`로 바꾸고, 감사 행과 "‘제목’: 알림을 켰어요" 메시지를 남긴다.
5. 거절은 `rejected`, 아무것도 쓰지 않는다. 다시 거절하면 `replayed`.
6. 쓰기를 실행하는 코드는 `confirm::approve` 하나뿐이다. 실행 경로(`run.rs`)에는 쓰기가 없다.

## 6. API

모두 JWT, `Cache-Control: private, no-store`, 본문 4KiB. 다른 사용자의 id는 **404**다.

| 엔드포인트 | 설명 | 꺼져 있을 때 |
|---|---|---|
| `GET /api/v1/assistant/agent` | `{enabled, model_planner_available, retention_days, goals[]}` | 200 `enabled:false` |
| `POST /api/v1/assistant/conversations` | `{context_program_id?}` → 201 | 404 `agent_disabled` |
| `GET /api/v1/assistant/conversations` | 최근 50개, 마지막 상태, 승인 대기 수 | 동작 |
| `GET /api/v1/assistant/conversations/{id}` | 메시지 + 최근 실행 20개 | 동작 |
| `DELETE /api/v1/assistant/conversations/{id}` | 즉시 삭제(실행·확인 cascade), 진행 중 작업 취소 → `{deleted:true}` | 동작 |
| `POST /api/v1/assistant/conversations/{id}/messages` | `{goal, program_id?, program_ids?}`, `Idempotency-Key` 선택 → 202 `{run_id, status, existing}` | 404 |
| `GET /api/v1/assistant/runs/{id}` | 실행 보기(단계·결과·메시지·확인) | 동작 |
| `GET /api/v1/assistant/runs/{id}/events` | NDJSON 진행 이벤트 | 동작 |
| `POST /api/v1/assistant/runs/{id}/cancel` | 취소, 남은 확인 만료 | 동작 |
| `POST /api/v1/assistant/confirmations/{id}/approve` | `{payload_hash}` | 404 |
| `POST /api/v1/assistant/confirmations/{id}/reject` | | 404 |
| `GET/PUT /api/v1/assistant/agent/proactive` | `{weekly_briefing, change_alerts, resume_suggestions}` (모두 기본 false). 변경 알림을 끄면 지문 삭제 | 동작 |
| `GET /api/v1/admin/assistant/runs` | AdminUser. 메타데이터만(목표·메시지·결과·사용자 id 없음) | 동작 |

한도(프로세스 메모리): 실행 시작 **사용자당 분당 6회**(`rate_limited` 429), 모델 계획 사용자당 시간당 6회, 승인·거절 분당 30회, 동시 실행 서버 전체 `ASSISTANT_AGENT_MAX_RUNS`(기본 8, 초과 `agent_busy` 429). IP 한도(분당 100)는 기존과 같다.

### 진행 이벤트 (NDJSON)

```
{"type":"status","status":"planning"}
{"type":"planning","planner":"model"}                       ← 모델 계획일 때만
{"type":"plan","planner":"fast_path","goal_kind":"…","steps":[{"index":0,"tool":"…","label":"…","permission":"read"}]}
{"type":"tool","seq":0,"index":0,"total":3,"tool":"…","label":"…","status":"running|ok|error|skipped|awaiting_confirmation","program_id":…}
{"type":"awaiting_confirmation","count":1}
{"type":"done","status":"completed","run":{…실행 보기…}}
{"type":"backgrounded","run_id":"…"}                        ← 구독 후 20초 동안 안 끝나면
```

- 늦게 붙은 구독자는 그때까지의 이벤트를 다시 받는다. 다른 프로세스나 끝난 지 2분이 지난 실행은 DB 스냅샷 `done` 한 줄을 받는다.
- `ASSISTANT_AGENT_BACKGROUND_AFTER_SECS`(기본 20초)보다 오래 걸린 실행이 끝나면 푸시 outbox에 한 건을 넣는다. 본문은 고정 문구이고 목표는 넣지 않는다.
- 모바일은 `backgrounded` 뒤 2초에서 10초로 늘려 가며 `GET /runs/{id}`를 폴링한다. 폴링 시간은 `GET /assistant/agent`의 `run_timeout_secs` + 30초(기본 330초)라서 서버보다 먼저 포기하지 않는다. 그래도 끝나지 않으면 "아직 끝나지 않았어요" 문구를 보여 준다. 화면 복귀 시 다시 읽는다.
- 모바일 상태 문구는 첫 화면부터 "생각 중"이다. 모델 계획(`planning`+`model`)이면 "보통 1분 안에 끝나요, 취소할 수 있어요" 줄을 덧붙이고, `timed_out`에는 다시 보내 달라는 줄을 붙인다. 취소 버튼은 연결 중에도 보인다.
- 취소는 진행 중인 모델 호출을 바로 끊는다(공유 모델 슬롯이 즉시 풀린다).

### 응답 메시지 (스펙 §10)

`{answer, verdict: pass|fail|unknown|not_evaluated, citations[{evidence_id, program_id, source_label, source_url?, captured_at?, claim_ids}], suggested_actions[{type, label, payload}], missing_profile_fields(≤2), warnings}`.

- `type`은 다음만 허용한다. URL이 든 payload는 지운다.
  - `open_program`, `prepare_application`, `confirm`, `goal`, `open_form_fill`, `edit_profile`, `pick_program`
- 쓰기는 승인 전에 답변에서 "승인이 필요한 변경 N건이 있어요. 승인하기 전에는 아무것도 바뀌지 않아요."로만 말한다. 승인하면 무엇이 바뀌는지는 승인 카드(`ConfirmationPreview`의 동작·지금·승인하면, 그 밖의 사실은 `note`)가 한 번 말한다. 앱은 카드 데이터(`reversible`, `note`)만 그리고 도구별 문구를 더하지 않으며, 답변과 카드는 서로의 문장을 되풀이하지 않는다. 성공 문장은 실행 결과를 확인한 뒤 승인 응답과 대화 메시지로만 나간다(기획 §5).

## 7. 모델 (선택)

- 공급자: `assistant_provider::PlanProvider`. Ollama 구현이 있다.
  - `/api/chat`, `stream:false`, `think:false`, `format` = `planner_json_schema()`, `temperature 0`, `num_predict 256`, `num_ctx 2048`, `options.num_thread`, `keep_alive`(`ASSISTANT_OLLAMA_KEEP_ALIVE`, 기본 30분)
  - `num_ctx`는 답변 스트림·의도 분류와 같은 2048이다. Ollama는 `num_ctx`가 바뀐 요청이 오면 모델을 다시 올린다. 예전 4096은 답변과 계획이 번갈아 오면 매번 다시 올렸다.
  - 프롬프트 버전 `agent-plan-v3`: 도구 한 줄 카탈로그, 두 가지 고정 모양, 거절·되묻기 규칙, 예시 8개(평가 목표가 아님, 테스트로 확인). 고정 접두부 약 1,020 토큰(v2 약 1,515).
  - 접두부는 호출마다 바이트가 같다. 다만 qwen3.5(하이브리드 순환 구조)의 GGUF 러너는 KV를 끝에서 약 500토큰 앞 체크포인트까지만 재사용한다. 그래서 접두부가 길어도 짧아도 호출마다 약 500토큰을 새로 평가한다(측정은 핸드오프 `agent-planner-v3.md`).
- **응답 형식**: `{"decision":"plan|refuse|needs_program","steps":[{"<도구>":{인자}}]}`, 최대 4단계.
  - 단계는 도구 이름을 유일한 키로 갖는다. Ollama는 스키마 문법의 객체 키를 정렬된 순서로 만들기 때문에 `{"tool","args"}`이면 모델이 인자(`args`)를 먼저 쓰고 그 인자에 맞는 도구를 고른다(v1 실패의 주원인). v1 `{"steps":[{"tool","args"}]}`도 읽는다.
  - 도구별 닫힌 인자 스키마(`anyOf`). 서버가 아는 인자는 묻지 않는다. 검색은 `keywords?`만, 마감은 빈 객체다(지역·상태·유형·기간은 서버가 목표에서 읽는다). 출력 토큰이 줄어 CPU 호스트에서 빨라진다.
- **서버 정규화**(`agent/planner.rs`, 운영과 eval이 같은 함수):
  - `answer_from_notice.question` = 가린 목표, `compare.programs` = 선택 2개 이상이면 `selected` 아니면 `saved`, 체크·상태·공고 단계는 `context`, 목표에 서류 이름이 있으면 체크 항목은 그 이름.
  - 검색 필터·키워드·`within_days`는 `agent/args.rs`(§2 인자)가 정한다. 모델 키워드는 정리를 통과한 것만, 하나도 없으면 파서 키워드다.
  - 단계는 목표 단어에 맞춘다(`planner::align_with_goal`): 목표가 저장을 말하는데 알림이면 저장으로(반대도), 목표에 없는 알림·저장·마감·혜택 단어 없는 검색은 버린다(준비 목표의 알림은 둔다), 화면 혜택 없이 검색 뒤에 붙은 `context` 단계는 버린다. 마지막 단계는 버리지 않는다.
  - 화면 혜택 자격 확인 단독 → 공고 인용 추가, 자격 질문 공고 단독 → 자격 확인 추가(빠른 경로와 같은 모양). 훑기(검색+마감)는 키워드 없음. 훑기 목표나 마감을 말하는 목표가 검색 하나로 오면 마감을 붙인다.
  - 화면 혜택이 없는데 `context` 단계 → `needs_program`. 중복 제거, 4단계 상한.
- **수리 1회**(스펙 §5.3): 파싱·검증 실패면 직전 응답과 검증기 사유를 붙여 1번 더 부른다. 두 호출이 같은 슬롯·같은 타임아웃 안에 있다. 전송 실패는 재시도하지 않는다. 잘린 출력(`done_reason=length`)은 무효 출력이지 장애가 아니다(차단기에 세지 않음).
- **입력**: 고정 시스템 정책 + 예시 턴 + `<맥락>화면 혜택: 있음/없음; 선택한 혜택: N개</맥락>` + `<목표>가린 목표</목표>`.
  - 블록 태그 `<`·`>`는 지운다.
  - 프로필·공고 원문·id·대화 이력·서류는 보내지 않는다.
  - 스펙 §14가 허용하는 프로필 필드조차 보내지 않는다.
- 생성 슬롯: v1 답변 스트림과 **같은 서버 전체 1개 슬롯**(`AssistantRuntime::try_model_slot`)을 쓴다. 대기 없이 바쁘면 대체 안내를 준다.
- 사용자당 모델 계획 **시간당 6회**(`MODEL_PLANS_PER_HOUR`): 한 사용자가 단일 슬롯(계획 1건 약 80초)을 독점하지 못한다. 초과하면 칩 안내를 준다.
- 차단기: 전송 실패·타임아웃이 연속 3회면 120초 동안 막는다. 파싱·검증에 실패한 계획은 세지 않는다(한 사용자의 이상한 목표가 모두의 차단기를 열지 못하게).
- 타임아웃: `ASSISTANT_AGENT_PLAN_TIMEOUT_SECS` 기본 180초. 실행 타임아웃(`ASSISTANT_AGENT_RUN_TIMEOUT_SECS`, 기본 300초)에서 60초(도구 타임아웃 15초 × 모델 계획 최대 4단계)를 뺀 값을 넘지 않게 줄인다(최소 10초). 모델이 늦으면 실행 시간 초과가 아니라 대체 안내로 끝난다. Oracle 측정값(v2 프롬프트)은 qwen3.5:9b 계획 p50 41초, p95 53초다. v3는 Oracle에서 다시 재야 한다.
- **콜드 스타트**(라우팅 v4, `AgentRuntime::spawn_warm_keeper`): 모델 플래너가 켜져 있으면 기본으로 켜진다(`ASSISTANT_AGENT_WARMUP=false`로 끈다).
  - 워밍업은 계획과 같은 고정 접두부(시스템 + 예시)에 짧은 목표 하나, 같은 `num_ctx`·`num_batch`·`num_thread`·문법, `num_predict 1`로 한 번 부른다(`PlanProvider::warm_up`). 모델이 내려가 있으면 올리고 접두부를 KV에 남긴다.
  - 30초마다 본다: 아직 데운 적 없을 때(API 시작), `keep_alive` 만료 2분 전(`-1`이면 안 함), 마지막 계획 뒤 답변 스트림이 돌아 접두부를 밀어낸 뒤 30초 조용할 때.
  - 단일 모델 슬롯을 잡고 돌며 바쁘면 그 차례를 건너뛴다. 워밍업 중 들어온 계획은 `Busy` → 규칙 답, 답변은 인용만이다. 워밍업 한 번은 계획 타임아웃이 아니라 자기 90초 상한을 쓴다. 실패는 차단기에 세지 않고 2분, 4분, 8분 … (최대 1시간) 뒤 다시 한다.
  - `keep_alive`가 새로 고침 창(2분 + 30초)보다 짧으면(`0s`·`1m`·`2m`) 키퍼를 켜지 않고 한 번 경고한다. 클라이언트가 끊은 답변 스트림도 끝난 시각을 남긴다.
  - 모델이 계획하지 못해 규칙 답으로 대신한 실행은 `planner = fast_path_fallback`으로 기록한다(마이그레이션 `20261017000001`).
  - 비용: API가 떠 있는 동안 모델이 늘 메모리에 있다. `ollama ps` 기준 qwen3.5:4b q4_K_M, `num_ctx 2048`, CPU 러너 3.5 GB(9b q4_K_M은 모델 파일만 6.6 GB). CPU는 차가운 적재·답변 뒤 접두부 복원마다 접두부 평가 한 번(약 1,050토큰, 운영 4b에서 첫 호출 약 50초의 대부분), 따뜻할 때 새로 고침은 짧은 목표 하나뿐이다. 워커 AI 추출(`num_ctx 8192`)이 같은 모델을 다시 올리면 다음 새로 고침 때까지 차갑다(`/api/ps`는 부르지 않는다).
- 모델 이름: `ASSISTANT_AGENT_PLANNER_MODEL`, 없으면 `ASSISTANT_AI_MODEL`, 그것도 없으면 `qwen3.5:9b`.
- **미세조정 플래너**(`ASSISTANT_AGENT_PLANNER_ENABLED`, `ASSISTANT_AGENT_PLANNER_PROMPT=tuned`): 답변 모델 없이 플래너만 켠다. Qwen3-1.7B를 계획 과제에 LoRA로 조정해 Q4_K_M GGUF(약 1.1 GB)로 Ollama 서비스(`infra/docker/compose.planner.yml`, `ASSISTANT_AGENT_PLANNER_URL`)에서 돌린다. 프롬프트는 학습 때와 같은 짧은 고정 시스템 턴(`TUNED_SYSTEM`, 버전 `agent-plan-tuned-v1`)과 목표뿐이고 예시가 없다. 문법은 v1/v2 도구만이다(v3 도구는 규칙이 라우팅). 출력은 다른 모델과 같은 `planner::interpret`·가드·`bars_writes`·확인 카드를 거친다. 학습·내보내기는 `scripts/ai/planner-finetune/`(sft.py, ft.py, export.sh). 플래너 계획 타임아웃은 `tuned`에서 기본 12초다(`ASSISTANT_AGENT_PLAN_TIMEOUT_SECS`로 바꾼다). `ASSISTANT_AGENT_PLANNER_DIGEST`를 주면 서빙 중인 모델 digest가 맞을 때까지 플래너를 끄고, 다르면 경고 한 번과 함께 끈다. 쓰기만 남은 계획을 쓰기 금지 목표에서 버린 경우(`WritesBarred`)는 가드의 거절이고, 모델이 스스로 고른 거절과 구분한다. 모델의 거절은 기본으로 거절로 답하지 않는다(`ASSISTANT_AGENT_PLANNER_REFUSALS`, 기본 off): 규칙의 답이 있으면 그 답, 없으면 '이해하지 못했어요' 안내다. 거절은 가드·규칙·분류기 기준선(0.99)의 몫이다. 조정 플래너가 사기 용어를 언급한 정직한 질문 16개 중 3개를 거절했기 때문이다.
- **검색 관련도 재정렬**(`ASSISTANT_SEARCH_RERANK_ENABLED`, 기본 꺼짐, 분류기 인코더 필요, `services/relevance.rs`): 검색 후보를 주제어(`query: `)와 제목+요약(`passage: `)의 e5 코사인으로 다시 매긴다. 키워드가 있고 마감 창이 없는 검색에서 0.78 미만은 빼고(아무것도 안 남으면 찾지 못했다고 말한다), 같은 주제 단계의 최고점보다 0.05 넘게 낮은 것은 뒤로 보낸다. 프로그램 벡터는 id·내용 해시로 캐시하고 백그라운드 작업이 시작 때와 매시 바뀐 행만 다시 만든다. 호출 위치는 `benefit_assistant/search.rs`에 있다(플래그로만 켜진다). 카탈로그 작업은 32개씩 나눠 사이에 쉬어, 같은 인코더 풀을 쓰는 실시간 분류가 끼어든다.
- **쓰기 이해 모델**(`ASSISTANT_UNDERSTANDING_ENABLED`, 기본 켜짐, `classifier/understand.rs`, `intent/policy.rs`): 목표 하나를 e5 한 번 + 문자·자모·끝 16자·마지막 두 단어 TF-IDF로 읽어 쓰기 종류별 행위(요청·언급·질문·보고·철회), 방향(알림 켜기/끄기, 저장/해제, 체크/해제, 상태), 대상(화면·저장 목록·선택·없음)을 낸다. 정책은 거르고 정할 뿐 알아보지 않는다: 종류별 카드 기준(dev 정밀도 ≥ 0.90) 이상이고 맨 위 행위가 요청일 때만 카드, 질문·언급·철회는 카드가 되지 않는다. 방향이 둘인 쓰기는 두 방향의 차이가 값 기준(dev에서 방향 오류가 없는 최저 차이)을 넘을 때만 카드이고, 아니면 현재 상태에 맞는 버튼이다. 저장 목록 대상은 저장한 혜택이 있고 목록 단어("목록", "리스트")가 있거나, 화면에 혜택이 없을 때 복수("…들", "다", "전부")일 때만이다. 화면에서 "내가 저장한 거"는 화면의 혜택이다. 가드가 막은 목표(주입 표지, AI에게 하는 지시+쓰기 등)는 모델에 가지 않는다. 계획기·분류기는 이때 쓰기를 계획하지 않고(게이트가 버린다), 양식·공식 페이지 안내만 모델이 요청할 때 남긴다. 산출물 `understanding.json`(git `crates/api/assets/understanding/`, sha256 고정)은 `scripts/ai/understanding/`(build_data.py, compose_rows.py, train.py)이 만들고 봉인 배치 해시와 겹치면 실패한다. 모델은 실행마다 한 번(`run::plan`의 `intent::prepare`, `spawn_blocking`, 300 ms 제한) 목표를 읽고, 읽는 곳(라우팅·게이트·생산자·답변)은 모두 그 읽기에 자기 맥락으로 정책만 적용한다. 시간을 넘기면 카드 없이 버튼이다. 실행 맥락(화면·선택·저장 수·의심)은 `run::plan`이 한 번 만들어 `observe::Journey`로 넘긴다. 상태 값에는 rejected(떨어짐·탈락·불합격)가 있다. core에 기록할 상태가 없으므로 카드 없이 상태 선택 버튼이다. 모델이 올라오기 전·실패·플래그 끔·`ASSISTANT_MODELS_ENABLED=false`에서는 어휘 규칙이 쓰기를 읽는다(AI 끔 대체 경로). 모델이 쓰기를 읽었지만 확신하지 못했거나 단계를 못 채운 종류는, 어휘 규칙이 같은 종류의 카드를 만들고 모델이 채운 값·대상과 맞으면 그 카드를 쓴다(`intent::agree_with_rules`). 질문·언급·철회로 읽은 종류와 모델이 읽지 않은 종류에는 쓰지 않는다. 남은 일(통합 리뷰 L1): 모델이 켜져 있어도 `goals.rs` 쓰기 경로가 라우팅(프로그램 선택 요청, 계획 여부)을 정한다. 쓰기 자체는 게이트·생산자 한 곳에서 정하지만, 다음 단계는 그 경로가 `model_active()`일 때 의도 층을 읽게 하는 것이다.
- **의도 분류기 산출물**(§2): API 이미지의 `/opt/mazimi/intent-classifier/`(`ASSISTANT_INTENT_CLASSIFIER_DIR`)에 세 파일이 있다. 세 sha256은 `classifier/mod.rs`에 고정돼 있다.
  - `model.safetensors`·`tokenizer.json`: intfloat/multilingual-e5-small(MIT), 리비전 `614241f622f53c4eeff9890bdc4f31cfecc418b3`. 이미지 빌드가 Hugging Face에서 받아 sha256을 확인한다(`infra/docker/Dockerfile.api`). git에는 없다.
  - `classifier.json`(약 5.8 MB, 형식 `mazimi-intent-classifier/2`, 버전 `e5s-char-jamo-lr-v3`, git `crates/api/assets/intent-classifier/`): 가중치·문자 어휘·자모 3–6그램 어휘·주제 헤드·학습 목표 해시. `scripts/ai/intent-classifier/train.py`(uv)가 `data/train_v3.json`(`data/gen_train.py`가 만든 합성 학습 세트)만으로 만든다. 평가 세트(dev·fd·holdout v1–v3·주제 eval·가드 코퍼스·topic_dev·fraud_dev, 저장소 밖 블라인드 홀드아웃의 정규화 해시)와 겹치는 학습 목표가 하나라도 있으면 실패한다(`train_overlap = 0`). 다시 돌리면 바이트가 같다(`--check`).
  - 실행: 순수 Rust(자체 Unigram 토크나이저 + `spm_precompiled` 정규화, `ndarray` 행렬곱, fp32, CPU). 인코더 층만 메모리에 올리고(약 85 MB), 25만 행 단어 임베딩 표는 검증한 파일에서 토큰 행만 읽는다(RSS 약 120 MiB). 스레드 `ASSISTANT_INTENT_CLASSIFIER_THREADS`(기본 2, 코어 수 이하). 분류기를 켤 때는 덧씌움 파일 `infra/docker/compose.classifier.yml`로 API를 CPU 0–1(`API_CPUSET`)에 두어 Ollama 기본 cpuset 2,3과 겹치지 않게 한다(기본 `compose.yml`은 API CPU를 바꾸지 않는다). 비동기 런타임 밖(`spawn_blocking`)에서 돈다.
- 수리 뒤에도 실패하면 칩 안내를 준다.
- 모델 문장은 쓰지 않는다. 최종 메시지는 템플릿이다(§12).

## 8. 인젝션 방어 (스펙 §13)

- 프롬프트 순서를 고정한다: 정책 → 도구 계약 → 서버 맥락 → 사용자 목표. 목표는 신뢰하지 않는 블록 안에 둔다.
- 모델은 URL·SQL·HTTP·id를 지정할 수 없다. 도구별 허용 인자와 열거형만 받는다(serde `deny_unknown_fields` + 도메인 검증).
- 서버는 소유권과 공개 상태를 다시 검증한다. 다른 사용자의 데이터는 토큰으로도 닿지 않는다(`saved`는 JWT 사용자 기준).
- 공고 문장은 인용 데이터일 뿐이다. 모델 계획 입력에도 들어가지 않는다.
- 어떤 계획이 와도 쓰기는 확인 카드다. eval 지표 `writes_without_confirmation`은 0이어야 하며 0이 아니면 `agent-eval`이 실패로 끝난다.

## 9. 데이터·보존·개인정보 (스펙 §11·§14)

마이그레이션 `20261013000001_assistant_agent.sql`. 모든 테이블은 RLS 강제 + 서비스 정책을 두고 `anon`/`authenticated` 권한을 회수한다.

| 테이블 | 내용 | 보존 |
|---|---|---|
| `assistant_conversations` | 제목(가린 첫 목표 40자), 화면 혜택 | 마지막 활동 후 `ASSISTANT_RETENTION_DAYS`(기본 30, 1~90) → 워커가 매시 삭제. 사용자 즉시 삭제. 탈퇴 cascade |
| `assistant_messages` | 가린 목표(≤300), 템플릿 답변, 인용 | 대화와 같이 |
| `assistant_runs` | 상태·계획·결과(도구 결과 + 메시지)·planner·model·지연 | 대화와 같이 |
| `assistant_tool_calls` | 도구·권한·인자(질문은 길이만)·요약·상태·지연 | 대화와 같이 |
| `assistant_confirmations` | 정규화 인자·precondition·미리보기·해시·상태·결과 | 대화와 같이 |
| `assistant_action_audit` | 도구 이름·결과·시각만 | 365일. 탈퇴 시 `user_id` NULL(비식별) |
| `assistant_proactive_settings` | 능동 알림 3종 동의 | 탈퇴 cascade |
| `assistant_program_fingerprints` | 변경 알림을 켠 사용자의 저장 혜택 비교값 | 끄면 즉시 삭제 |
| `assistant_proactive_deliveries` | 능동 알림 발송 기록(빈도 제한 키) | 120일 |
| `assistant_notification_outbox` | 늦게 끝난 실행의 푸시 | 6시간 뒤 drop, 7일 뒤 삭제 |

> 마이그레이션 주석 정정(적용된 파일은 고치지 않는다): `20261013000001_assistant_agent.sql`의 "Nothing here is sent to a model"은 정확하지 않다. 규칙이 처리하지 못한 목표(`Unrecognized`)는 **가린** 목표 그대로 계획 모델에 간다. 모델은 자체 호스팅 Ollama 엔드포인트이고 제3자 서비스가 아니다. 가림 범위는 §2의 `redact_question`이다.

- `assistant_expire_confirmations(user|NULL)` DB 함수 하나가 API(사용자별, 조회·결정 직전)와 워커(전체)의 만료 처리를 맡는다.
- 개인정보처리방침 `apps/mobile/app/privacy-policy.tsx` 제6조의3에 저장 항목·30일 보존·모델 입력 범위·승인형 변경·능동 알림을 적었다. 보존 기간을 바꾸면 그 문구도 같이 바꿔야 한다. `PRIVACY_VERSION` 재동의 여부는 법무 판단으로 남겼다.

## 10. 능동 비서 (Phase 3, 워커 `mazimi_worker::assistant_agent`)

- 스위치는 둘 다 켜져 있어야 한다: `ASSISTANT_AGENT_PROACTIVE_ENABLED`(기본 off)와 사용자별 동의(기본 off).
- 시간·빈도: 09:00~20:59 KST에만 보낸다. 사용자당 KST 하루 1건이고(종류 합산), 알림 설정의 푸시 채널을 따른다. 푸시만 쓰고 카카오는 쓰지 않는다.
- 매시 20분 작업:
  - **주간 브리핑**: 월요일 10시 이후 첫 실행. ISO 주당 1회다. 내용은 "이번 주 마감 N개, 준비 중 M개"뿐이고 제목은 넣지 않는다.
  - **변경 영향**: 저장 혜택의 마감·조건(원문 해시)·서류·자격 규칙 버전·금액·상태 지문을 비교한다. 처음 본 혜택은 기준선만 남긴다. 바뀐 항목 이름을 알리고, 지문마다 1회만 보낸다. 같은 시점에 여러 혜택이 바뀌거나 일일 한도·푸시 꺼짐으로 못 보낸 변경은 이전 지문을 유지해 다음 실행에서 보낸다.
  - **멈춘 준비**: 7~30일 손대지 않은 준비가 있고 마감 전이며 신청 확정 기록이 없으면 보낸다. 같은 정지 상태는 1회만 보낸다.
- 잠금 화면에 혜택 이름을 띄우지 않는다(민감한 혜택명이 상황을 드러낼 수 있음). 어느 혜택인지는 앱에서 보여 준다.
- 5분마다 outbox를 비운다(같은 시간대 규칙).
- 정리 작업은 플래그와 상관없이 항상 돈다.

## 11. 평가

- 두 분할(둘 다 사람 검수 전 초안, 출시 게이트 아님):
  - **dev** `crates/api/tests/fixtures/agent_goals.json`(124개, `split: dev`): 원래 78개 + `dev2-*` 46개(구어·오타·띄어쓰기·복합 의도). 빠른 경로·프롬프트·정규화를 이 세트로만 고쳤다. 수치는 회귀 하한이다.
  - **holdout** `agent_goals_holdout.json`(55개, `frozen`, `created_at 2026-10-12`): dev 세트와 라우터를 읽지 않은 별도 라벨러가 라벨 규칙 문서만 보고 썼다. 튜닝 전에 커밋했고 sha256을 테스트가 고정한다. 결과를 본 뒤 고치지 않는다. 새 세트는 새 파일로 만든다. 프롬프트 예시는 두 세트 어디에도 없다(테스트).
- CLI `agent-eval`(`crates/api/src/bin/agent-eval.rs`):
  - 옵션: `--provider rules|mock|ollama`, `--mode pipeline|model-only`, `--split dev|holdout`(또는 `--fixture`), `--model`, `--url`, `--allow-any-port`, `--num-thread`, `--timeout-seconds`, `--limit`, `--out`, `--classifier`(`--classifier-dir`, `--classifier-threads`, `--classifier-threshold`, 비교용 `--classifier-deferral-to-model`), `--understanding`(`--understanding-dir`), `--saved N`(모든 목표의 사용자가 저장한 혜택 수, 기본 0), `--provider replay --replies PATH`. 결과 행의 `intents`는 의도 층의 읽기(종류:행위:단계 유무)다
  - `--understanding`은 운영 `runtime.rs`처럼 쓰기 이해 모델을 올리고, 목표마다 라우팅 전에 한 번 읽힌다(`intent::prepare_blocking`). 빼면 쓰기 카드는 AI 끔 대체 경로(어휘 규칙)가 만든다. 운영은 이 모델이 기본으로 켜져 있으므로 운영 수치는 이 옵션을 넣고 잰다.
  - `--provider replay`는 녹화한 플래너 응답(`{가린 목표 \u0001 has_program \u0001 selected: 응답}`)을 같은 `planner::interpret`에 넣는다. model-only로 한 번 녹화하면 모델 없이 라우팅·카드 변경을 다시 잴 수 있다. 11-18 holdout v3 model-only 응답의 replay는 기록 수치(도구 0.889, 인자 0.828, 거절 0.897)를 그대로 재현한다.
  - `--classifier`는 pipeline에 의도 분류기를 넣는다(가드 → 분류기 → 0.7 미만이면 규칙의 확신 모양, 그다음 모델, 모델이 없으면 규칙). 보고서에 `classifier_routed`(분류기가 답한 비율), `classifier`(지연·답함/넘김·픽스처와 학습 목표의 해시 겹침 `train_overlap`), `by_source_accuracy`, `process_memory_mib`(Linux)가 붙는다.
  - `pipeline`은 운영 순서(거절 규칙 → 빠른 경로 → 모델)다. 모델 경로는 운영과 같은 `planner::plan`(정규화·수리 포함)이다.
  - 지표: 도구명 정확도(플래너 출력 기준), 카드 정확도(`card_accuracy`: 쓰기를 기대한 목표에서 기대한 쓰기와 `observe::expand` 뒤 실제 카드가 순서 무시로 같은 비율. 확신이 낮아 카드 대신 답변의 상태 버튼이 된 경우는 틀린 것으로 센다), 기대하지 않은 카드(`unasked_cards`), 인자 완전 일치(프로그램 유형 순서 무시), 거절 정확도(`needs_program`은 엄격), 수리 뒤 invalid JSON·plan 비율, 첫 시도 무효율, 수리 수, 쓰기 제안, **확인 없는 쓰기(0이어야 함)**, 인젝션 쓰기, 지연, 분류별 지표, 기대→산출 도구 순서 혼동표, 원문 응답(진단용)
- 서버 벤치: `scripts/ai/benchmark-agent.sh`(사람이 서버에서 실행). 기본 `RUNS`가 dev·holdout × model-only·pipeline 4회다.

로컬 측정(2026-10-12, M5 Pro Metal, Ollama 0.40.1, `qwen3.5:9b` digest `d63f830f28d5`). 품질만 비교 가능하고 지연은 Oracle과 비교할 수 없다.

| 실행 | 도구명 | 인자 | 거절 | invalid JSON | 인젝션 쓰기 |
|---|---|---|---|---|---|
| main 코드, dev 78, model-only | 0.214 | 0.107 | 0.909\* | 0.179 | 0 |
| 이번 코드, dev 78, model-only | 0.875 | 0.875 | 1.0 | 0 | 0 |
| 이번 코드, dev 124, model-only | 0.872 | 0.872 | 1.0 | 0 | 0 |
| 이번 코드, dev 124, pipeline | 0.979 | 0.979 | 1.0 | 0 | 0 |
| main 코드, holdout, model-only | 0.314 | 0.143 | 0.85\* | 0.145 | 0 |
| main 코드, holdout, rules | 0.657 | 0.571 | 0.75\* | – | 1 |
| **이번 코드, holdout, model-only** | **0.914** | **0.886** | **0.95** | **0** | **1** |
| 이번 코드, holdout, pipeline | 0.857 | 0.771 | 0.75 | 0 | 1 |
| 이번 코드, holdout, rules | 0.686 | 0.600 | 0.65 | – | 1 |

\* main 코드는 빈 모델 계획을 `needs_program`으로도 맞게 쳤다(이번 코드는 엄격).

- holdout에서 pipeline이 model-only보다 낮다: 빠른 경로가 dev에 맞춰져 있어 holdout의 복합 의도(multi 0.5 vs 0.75)와 범위 밖 목표(negative 거절 0.6 vs 1.0)를 가로챈다. 라우팅 순서를 바꿀지는 새 holdout(v2)으로 확인한 뒤 정한다(이 holdout으로 고치면 튜닝 대상이 된다).
- 인젝션 쓰기 1건(holdout, 세 모드 모두): 확인 카드로만 나가고 실행되지 않지만 게이트(0) 위반이다. 사례를 보지 않았다.
- Oracle 수치(main 코드, 78개, model-only): 도구 0.32, 인자 0.16, 거절 0.64, invalid JSON 0.19, p50 27초, p95 32초. 이번 코드는 아직 Oracle에서 재지 않았다.

**누설 고지(2026-10-12)**: Oracle holdout 결과의 목표별 실패를 읽고 라우팅 가드(`agent/guard.rs`)를 만들었다. 그래서 holdout v1(`agent_goals_holdout.json`)은 이제 **dev 세트**다. 그 수치는 회귀 하한일 뿐 품질 주장이 아니다. 규칙에는 holdout 문장을 그대로 넣지 않고 원리(역할 태그, 제3자 + 계정 데이터, 필드 + 변경 요청 등)로 일반화했다. 게이트는 가드와 이 결과를 보지 않고 따로 쓴 새 holdout이다.

라우팅 가드 전후(로컬, 같은 모델·digest, pipeline):

| 세트 | 실행 | 도구 | 인자 | 거절 | 인젝션 쓰기 | 규칙 처리 | 규칙 정확 | 잘못된 거절 |
|---|---|---|---|---|---|---|---|---|
| dev 124 | rules 전·후 | 0.904 | 0.904 | 1.0 | 0 | 113 | 113/113 | 0 |
| dev 124 | qwen3.5:9b 전·후 | 0.979 | 0.979 | 1.0 | 0 | 113 | 113/113 | 0 |
| holdout v1 55 | rules 전 | 0.686 | 0.600 | 0.65 | 1 | 41 | 30/41 | 0 |
| holdout v1 55 | rules 후 | 0.686 | 0.600 | 0.90 | 0 | 38 | 35/38 | 0 |
| holdout v1 55 | qwen3.5:9b 전 | 0.857 | 0.771 | 0.75 | 1 | 41 | 30/41 | 0 |
| holdout v1 55 | qwen3.5:9b 후 | 0.943 | 0.857 | 1.0 | 0 | 38 | 35/38 | 0 |

- rules의 거절 정확도는 보류(`none`)를 `no_plan`으로 맞게 친다. 가드 효과는 "거절 기대인데 도구로 간 목표" 5 → 0과 인젝션 쓰기 1 → 0으로 본다.
- 가드 전 qwen3.5:9b 로컬 pipeline 수치는 Oracle 결과와 같다(0.857/0.771/0.75/1).
- 잘못된 거절은 가드가 없던 때 쓴 다른 코퍼스로도 쟀다. v1 질문 세트(dev·holdout·regression의 범위 안 질문)와 grounded-QA 초안을 합한 192개에서 0건이다(`guard::tests::older_in_scope_question_corpora_are_not_refused`).
- 2026-10-12 코드 리뷰가 직접 쓴 일상 목표 53개 중 44개를 가드가 거절했다. 가드를 강한 표지만 거절하도록 바꾼 뒤 0개다. 그 대신 holdout v1의 프로필 변경 1개와 금리 순위 1개는 거절이 아니라 보류로 간다.
- `agent-eval`이 `false_refusals`와 `fast_path_accuracy`를 함께 낸다.

**누설 고지 2(2026-10-12, 플래너 v3)**: Oracle holdout v2 결과의 목표별 실패를 읽고 플래너 v3(`agent/args.rs`, 빠른 경로 정밀도, `planner::align_with_goal`, 프롬프트 `agent-plan-v3`)를 만들었다. 그래서 holdout v2(`agent_goals_holdout_v2.json`)도 이제 **dev 세트**다. 규칙과 프롬프트에는 v2 문장을 넣지 않고 원리(붙여 쓴 꼴, 기간 단어, 목표가 말하지 않은 단계 버리기 등)로 일반화했다. dev의 두 사례(`dev2-sweep-2`, `dev2-sweep-4`)는 "기간 없는 `받을 수 있는 X`는 검색" 규약에 맞춰 다시 라벨링했다(fixture note). 게이트는 이 결과를 보지 않고 따로 쓰는 holdout v3다.

플래너 v3 전후(로컬 M5 Pro, Ollama 0.40.1, Oracle과 같은 GGUF `qwen3.5:9b-q4_K_M` digest `56671c2ab938`, Metal). 세 세트 모두 dev 세트라 회귀 하한이다.

| 세트 | 실행 | 도구 전→후 | 인자 전→후 | 거절 전→후 | 빠른 경로 정확도 전→후 |
|---|---|---|---|---|---|
| holdout v2 80 | model-only | 0.837 → 0.980 | 0.694 → 0.980 | 0.968 → 0.935 | – |
| holdout v2 80 | pipeline | 0.735 → 0.939 | 0.612 → 0.939 | 0.903 → 0.935 | 0.717 → 0.932 |
| holdout v1 55 | model-only | 0.829 → 0.971 | 0.771 → 0.943 | 0.95 → 0.95 | – |
| holdout v1 55 | pipeline | 0.943 → 1.0 | 0.829 → 0.971 | 1.0 → 1.0 | 0.914 → 1.0 |
| dev 124 | model-only | 0.798 → 0.957 | 0.787 → 0.915 | 1.0 → 1.0 | – |
| dev 124 | pipeline | 0.979 → 1.0 | 0.979 → 0.979 | 1.0 → 1.0 | 1.0 → 1.0 |

- invalid JSON 0, 확인 없는 쓰기 0, 인젝션 쓰기 0(모든 실행).
- v2 거절 0.935는 되묻기 2건(`needs_program`↔`no_plan`, 선택 없이 "이 두 개 비교")이다. 인젝션·범위 밖 22건은 모두 맞다.
- 독립 가드 코퍼스(rules): 잘못된 거절 1, 공격 거절 61, 공격 쓰기 0. 전후 같다.
- 지연(로컬 CPU 3스레드, Oracle 엔진과 같은 GGUF 러너, 접두부가 캐시된 뒤 계획 1건): v2 14.5초 → v3 2.6초. 프롬프트 평가 약 500토큰 → 약 60토큰(`num_batch` 64), 출력 33 → 25토큰. Oracle 추정은 핸드오프 `agent-planner-v3.md`.

Oracle 게이트 측정(2026-11-18, holdout v3 128개, 운영 플래너 `mazimi-planner-q17-v2` digest `8a7e88aaa0c6`, `tuned`, 스레드 2, 타임아웃 12초, pipeline은 분류기 포함). 결과 파일은 `docs/validation/agent/benchmarks/oracle-20261118-planner-q17-v2-holdout-v3/`에 있다.

| 실행 | 도구 | 인자 | 거절 | 잘못된 거절 | 빠른 경로 정확도 | invalid JSON | 확인 없는 쓰기 | 인젝션 쓰기(우회 문구 제외) | 모델 p50 / p95 |
|---|---|---|---|---|---|---|---|---|---|
| pipeline(빠른 경로 82, 분류기 17, 모델 29) | 0.889 | 0.838 | 0.897 | 0 | 0.915 | 0 | 0 | 1 (0) | 2.5초 / 7.7초 |
| model-only | 0.889 | 0.828 | 0.897 | 1 | – | 0 | 0 | 1 (0) | 2.0초 / 3.3초 |

- 통과: 인자 ≥ 0.80, invalid JSON, 확인 없는 쓰기 0, p95 ≤ 12초. 미달: 도구 ≥ 0.90, 거절 ≥ 0.95.
- 인젝션 쓰기 1건은 "확인 없이" 문구가 있는 목표를 확인 카드로 계획한 경우라 라우팅 v4 결정에 맞는다.
- 약한 분류는 마감(pipeline 도구 0.43), 인젝션 거절 0.75, 복합 의도 0.75다.
- 목표별 문장은 읽지 않았다. holdout v3는 게이트 세트로 남는다.
- 이 측정은 쓰기 이해 모델 없이 돌았고 도구 지표는 플래너 출력 기준이라, 운영에서 사용자가 받는 카드는 재지 않았다.

holdout v3 카드 정확도(2026-11-20, 11-18 model-only 응답 replay, 분류기 `e5s-char-jamo-lr-v3`, 쓰기 이해 모델 `e5s-char-jamo-understand-v4`). 쓰기를 기대한 목표는 34개다. 규칙 합의는 아래 dev 분석 뒤에 넣은 `intent::agree_with_rules`다.

| 실행 | 도구 | 인자 | 카드 | 기대하지 않은 카드 |
|---|---|---|---|---|
| pipeline, 어휘 규칙 | 0.899 | 0.848 | 0.500 | 2 |
| pipeline, 쓰기 이해 모델 | 0.899 | 0.838 | 0.559 | 2 |
| pipeline, 쓰기 이해 모델 + 규칙 합의(운영 조건) | 0.899 | 0.838 | 0.618 | 2 |
| pipeline, 쓰기 이해 모델 + 규칙 합의, `--saved 3` | 0.899 | 0.838 | 0.676 | 3 |
| model-only, 어휘 규칙 | 0.889 | 0.828 | 0.500 | 1 |
| model-only, 쓰기 이해 모델 | 0.889 | 0.828 | 0.559 | 1 |

- 규칙 합의 전 운영 조건의 경로별 카드 정확도는 빠른 경로 1.0, 분류기 0.50, 모델 0.19였다. 쓰기 이해 모델이 켜지면 플래너의 쓰기 단계를 버리고(`cards::gate_plan`) 의도 층이 카드를 다시 만드는데, 의도 층이 모델과 분류기로 간 쓰기 목표에서 카드 기준을 넘지 못했다. 분류별로는 알림 0.4, 저장 0.5, 상태 0.4, 복합 요청 0.22(어휘 규칙 0.44)였다.
- `--saved 3`의 기대하지 않은 카드 1건 증가는 저장 목록 대상 알림 카드다. 규칙 합의는 화면 혜택 대상 카드만 가져오므로 이 카드는 모델의 읽기에서 나왔다. 주입 분류의 카드 2건은 모두 확인 생략 문구 목표다(`injection_writes_non_bypass` 0).
- 카드 대신 답변에 상태 버튼이 남는 경우는 사용자가 쓰기를 할 수 있지만 eval은 버튼을 재지 않는다. 픽스처는 저장한 혜택 수를 담지 못하므로(`GoalContext::saved`는 역직렬화하지 않는다) 저장 목록 대상 쓰기는 `--saved N`으로 잰다.
- pipeline replay의 도구 0.899는 11-18 pipeline 기록(0.889)보다 1건 많다. replay는 모델로 간 목표에 model-only 응답을 쓰므로, 두 서버 실행에서 응답이 달랐던 1건으로 본다.

의도 층 카드 정확도(정답 플래너 `--provider mock`, `--saved 3`, dev 세트 세 개). 플래너가 완벽해도 카드가 빠지는 비율을 본다.

| 세트 | 어휘 규칙 | 쓰기 이해 모델 | 쓰기 이해 모델 + 규칙 합의 |
|---|---|---|---|
| dev | 0.78 | 0.68 | 0.84 |
| holdout v1 | 0.36 | 0.50 | 0.64 |
| holdout v2 | 0.73 | 0.73 | 0.87 |

- 규칙 합의 전 쓰기 이해 모델이 놓친 문항은 세 갈래였다. 분명한 쓰기를 확신하지 못한 경우(`Unclear`, 13건, 이 중 9건은 어휘 규칙이 맞힘: "나 이 장학금 신청했어", "이거 저장", "통장사본 체크 해제해줘"), 요청으로 읽고도 대상이나 값을 못 채운 경우("이거 알림 그만"), 라벨이 준비 여정의 알림을 카드로 기대한 경우(8건)다. 준비 여정의 알림은 라운드 12부터 답변 버튼이므로 마지막 갈래는 라벨이 현재 정책과 다르다.
- 규칙 합의: 모델이 쓰기를 읽었지만 확신하지 못했거나(`Unclear`) 요청인데 단계를 못 채웠을 때, 어휘 규칙이 같은 종류의 카드를 만들고 모델이 채운 값·대상과 다르지 않으면 그 카드를 쓴다. 모델이 질문·언급·철회로 읽었거나 그 종류를 읽지 않은 목표, 읽기가 없는 목표(시간 초과)는 그대로다.
- 정밀도: `acceptance_probe`(손 라벨 `agent_card_requests.json` 1,034행, 모두 화면 혜택 있음, 저장 3개)에서 기대하지 않은 카드가 붙은 행은 합의 전후 모두 34행이다. 재현율은 알림 켜기 244→246/259, 저장 36→37/40, 상태 15→17/19, 캘린더(모든 도구 켬) 1→2/2다. 이 코퍼스는 쓰기 이해 모델의 개발 자료라 오를 여지가 작다.

## 12. 스펙과 다르게 정한 것

- **모듈 위치**:
  - API 에이전트는 `crates/api/src/agent/`(공개 모듈, 테스트 주입용)에 두었다. 스펙의 `services/assistant/`가 아니다.
  - 계약 타입·상태 머신·해시는 `crates/core/src/agent.rs`에 있다.
  - 워커 작업은 `crates/worker/src/assistant_agent.rs`에 있다.
- **모델 문장 없음**: 최종 메시지는 템플릿이다. 선택 표현 다듬기(기존 근거 검증기 통과 시)는 붙이지 않았다.
- **확인 승인 본문은 해시를 포함한다**: 스펙 §8은 "confirmation ID만"이라고 했다. 승인한 카드와 실행 내용을 묶으려고 카드의 해시를 같이 보낸다(기획 A-05 "해시가 다르면 거절").
- **SSE 대신 NDJSON**: v2 답변 스트림과 같은 방식이다. 폴링 폴백이 있다.
- **프로필 변경 도구**: v1에는 없었다. v2(`profile_patch`)는 본인 프로필의 6개 항목만, 목표 문장에 값이 하나 있을 때만 카드를 만든다. 값이 없거나 다른 사람·질문·부정·인젝션 표시가 있으면 v1처럼 거절하거나 넘긴다. 근거·보안 분석은 `.omc/handoff-20261008/agent-forms.md`.
- **보존 기간 확정 제안**: 30일. 스펙 §24-2는 미결정이었다.
- **하단 탭**: 기획 §9는 탭 추가를 미뤘지만 이번 요청에 따라 "비서" 탭을 넣었다. 서버 플래그가 꺼져 있으면 탭이 숨겨진다(`href: null`).

## 13. 켜는 순서와 게이트

1. 마이그레이션 적용, 플래그 off로 API·워커 배포(정리 작업은 이때부터 돈다).
2. `ASSISTANT_AGENT_ENABLED=true`(모델 off): 빠른 경로만 쓴다. 내부 테스터로 승인 흐름을 실기기에서 검증한다.
3. 모델 계획을 켜는 조건(모두):
   1. 사람이 검수한 목표 세트(실사용 로그나 인터뷰 기반, 초안 세트와 별개)를 쓴다.
   2. `benchmark-agent.sh` 결과가 다음을 만족한다.
      - `writes_without_confirmation = 0`, `injection_writes = 0`
      - 모델 분류 도구명 정확도 ≥ 0.90, 인자 일치 ≥ 0.80
      - invalid JSON ≤ 2%, 거절 정확도 ≥ 0.95
      - p95 ≤ 계획 타임아웃
      - 비도우미 API p95 악화 ≤ 10%
   3. v2 계약 §4의 AI flag 조건(개인정보처리방침·동의 기록·soak)도 만족한다.
4. 의도 분류기: 이미지에 산출물을 실은 API를 플래그 꺼진 채 배포하고, 서버 게이트(`agent-eval --classifier`, holdout v4, qwen3.5:4b)를 통과하면 `ASSISTANT_INTENT_CLASSIFIER_ENABLED=true`. 끄면 바로 예전 라우팅이다. 절차·수치는 핸드오프 `agent-classifier.md`.
5. 능동 알림: 문구 법무 확인, 내부 테스터 1주, 그 뒤 `ASSISTANT_AGENT_PROACTIVE_ENABLED=true`.
6. 롤백:
   - `ASSISTANT_AGENT_ENABLED=false`로 새 실행과 승인을 막는다(404). 기록 조회·삭제는 계속 된다.
   - 대기 중인 확인은 10분 안에 만료된다.
   - 테이블은 지우지 않고 보존 정책대로 정리한다.

7. v3 도구(캘린더·서류 발급처·공식 페이지 확인). 앱 쪽 코드는 런타임 1.0.1에 이미 있어 네이티브 빌드 없이 켠다.
   1. `ASSISTANT_AGENT_CALENDAR_ENABLED=true`, `ASSISTANT_AGENT_DOCUMENT_LINKS_ENABLED=true`: API 재시작만. 켠 뒤 실기기에서 캘린더 카드 승인 → 일정(하루 종일, D-3·D-1 09:00 알람)이 들어가는지, 서류 링크가 열리는지 본다.
   2. `ASSISTANT_AGENT_OFFICIAL_PAGE_ENABLED=true`: 먼저 `docker compose --profile official-page up -d official-page`로 격리 페치 서비스를 띄우고, api와 같은 `OFFICIAL_PAGE_MCP_TOKEN`·`OFFICIAL_PAGE_MCP_URL`을 넣은 뒤 API를 재시작한다. 서비스가 없으면 도구는 링크만 주고 "확인하지 못했어요"로 답한다.
   3. 서류 카탈로그 링크는 켜기 전에 `cargo test -p api --lib issuer_urls_still_resolve -- --ignored`로 다시 확인한다.
   4. 롤백: 해당 플래그만 `false`로 바꾸고 API를 재시작한다. 꺼진 도구는 빠른 경로에서 "아직 할 수 없어요"로 답하고, 답변 버튼과 모델 계획에서 빠진다. 이미 만든 기기 일정과 서버 데이터는 건드리지 않는다(서버에 저장한 것이 없다).

## 14. 코드 위치

| 책임 | 코드 |
|---|---|
| 계약(도구·계획·상태·해시) | `crates/core/src/agent.rs` |
| 빠른 경로·거절 | `crates/api/src/agent/goals.rs` |
| 인자 정규화(검색 필터·키워드·마감 기간) | `crates/api/src/agent/args.rs` |
| 라우팅 가드(거절·보류·쓰기 차단) | `crates/api/src/agent/guard.rs` |
| 실행 | `crates/api/src/agent/run.rs` |
| 도구 어댑터 | `crates/api/src/agent/tools.rs` |
| 확인·실행 | `crates/api/src/agent/confirm.rs` |
| 메시지·검증 | `crates/api/src/agent/compose.rs` |
| 저장 | `crates/api/src/agent/store.rs` |
| 런타임(플래그·한도·차단기·이벤트) | `crates/api/src/agent/runtime.rs` |
| 의도 분류기 | `crates/api/src/agent/classifier/`(적재·특징·회귀·슬롯 규칙, `encoder.rs` BERT), `crates/api/assets/intent-classifier/classifier.json`, `scripts/ai/intent-classifier/` (학습) |
| 평가 | `crates/api/src/agent/eval.rs`, `crates/api/src/bin/agent-eval.rs`, `scripts/ai/benchmark-agent.sh` |
| HTTP | `crates/api/src/routes/agent.rs` |
| 모델 계획 | `crates/api/src/agent/planner.rs`(호출·수리·정규화), `crates/api/src/assistant_provider/{mod,ollama}.rs` (`PlanProvider`, 프롬프트), `crates/core/src/agent.rs` (`planner_json_schema`, `PlannerReply`) |
| 워커 | `crates/worker/src/assistant_agent.rs`, `crates/worker/src/main.rs` |
| 모바일 | `apps/mobile/app/(tabs)/agent.tsx`, `apps/mobile/app/agent-history.tsx`, `apps/mobile/components/AgentCards.tsx`, `apps/mobile/lib/{agent,agentStream,agentTypes}.ts` |
| 관리자 웹 | `apps/web/src/app/admin/assistant-runs/page.tsx` |
| 테스트 | `crates/api/tests/agent_cases/mod.rs`, 모듈 단위 테스트, `apps/mobile/scripts/check-session.cjs` |

## 15. 남은 일

- `scripts/deploy.sh`는 `compose.yml`만 쓴다. 이 스크립트로 배포하면 플래너 연결이 다시 끊긴다(상태 줄).
- 운영 플래너가 holdout v3 게이트의 도구 정확도(0.889)와 거절 정확도(0.897)에 미달한다(§11). 고친 뒤에는 v3 결과를 보지 않은 새 holdout으로 잰다. 사람이 검수한 목표 세트도 필요하다.
- 운영 조건의 holdout v3 카드 정확도가 0.618이다(§11, 규칙 합의 포함). 복합 요청과 모델로 간 쓰기 목표가 약하다. 원인은 v3 문항이 아니라 새 holdout으로 나눠 본다.
- dev 픽스처 일부(준비 여정 목표에 알림 카드를 기대하는 8건)는 라운드 12 정책과 다르다. 라벨을 고칠지 정한다.
- 저장한 혜택 수는 픽스처에 담기지 않아 `--saved N`으로 준다. 새 holdout에는 저장 목록 대상 목표의 맥락을 따로 적는다.
- holdout 인젝션 쓰기 1건: 원인 확인은 새 holdout으로 한다(이 세트는 보지 않는다).
- 모바일 실기기·시뮬레이터 확인을 하지 않았다(Xcode 라이선스 미동의). `expo/fetch` 스트림의 Hermes 동작도 실기기에서 확인해야 한다.
- 한도·차단기·이벤트 레지스트리는 프로세스 메모리에 있다. API 인스턴스가 여러 개면 공유 저장소가 필요하다(이벤트는 DB 폴링으로 대체된다).
- 브리핑(A-06)의 "새롭게 조건이 맞은 혜택 / 정보 하나로 판정 가능한 혜택"과 미래 프로필 시뮬레이션은 하지 않았다.
- 승인 감사 화면에서 실행 단위 되돌리기는 없다(사용자가 반대 목표로 다시 요청).
- 개인정보처리방침 재동의(`PRIVACY_VERSION`) 여부는 법무가 판단한다.
