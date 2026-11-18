# 마지미 혜택 도우미 계약 (v2: 근거 검색형 Q&A)

> 상태: v2 계약 (2026-10-12 개정). 파일 이름은 링크 호환을 위해 그대로 둔다.
> 결정: 질문의 답은 **저장된 공고 원문을 인용**해서 만든다. 핵심 경로는 문장을 생성하지 않는다. 생성 요약(G)은 선택 기능이며 `ASSISTANT_AI_ENABLED=false`(기본)로 꺼져 있다.
> 이유: 목표 서버(Oracle ARM 3코어)에서 `qwen3.5:4b`가 프롬프트 약 23 tok/s, 출력 약 8.7 tok/s라 생성 답변이 30~100초 걸린다. 인용 답변은 모델 없이 즉시 나온다.
> 로드맵: [기술 스펙 — 후속 설계](mazimi-ai-assistant-technical-spec.md). 그 문서의 수치·모듈 배치는 이 계약이 아니다. 제품 범위: [제품 기획서](mazimi-ai-assistant-product-plan.md)

## 1. 동작 범위

`Expo → 인증된 assistant route → benefit_assistant / grounded_qa → structural·rule_engine + evidence + application_preparation → DB`.

| 엔드포인트 | 계약 |
|---|---|
| `GET /api/v1/programs/{id}/assistant` | 요약·조건·다음 할 일 스냅샷. `answer` 없음. 모델 호출 없음 |
| `POST /api/v1/programs/{id}/assistant` | `{question, allow_ai_processing?, previous_topic?}`. 1~500자, 추가 필드 422, 본문 4KiB. **사용자당 분당 10회**(생성 스트림과 합산), 초과 429. v1 필드 그대로 + `answer`(§2). 모델 호출 없음 |
| `POST /api/v1/programs/{id}/assistant/answer-stream` | 선택 생성 요약(§4). `allow_ai_processing: true` 필수. NDJSON 스트림 |
| `POST /api/v1/assistant/search` | `{question}`. 질문 하나로 전체 혜택 찾기(§5). **사용자당 분당 6회**. 모델 호출 없음 |
| `GET /api/v1/assistant/briefing` | 저장·준비·알림 구독 혜택 중 마감 순 최대 3개. 단일 read-only 스냅샷 |
| `GET /api/v1/assistant/bundles?scenario=…` | 저장 프로필 + 공통 추천(`recommendations::evaluate`)으로 상황별 3그룹 × 최대 3건. 금융상품 제외 |
| `POST /api/v1/assistant/compare` | `program_ids` 2~4개. 금융상품이 하나라도 있으면 400 |
| `POST /api/v1/stack-check` | **폐기 예정**. 형태 유지, 모든 쌍 `unknown`. 응답에 `Deprecation: true`, `Sunset: Sun, 31 Jan 2027 00:00:00 GMT` |

개인화 응답은 모두 `Cache-Control: private, no-store`. user_id는 JWT에서만 읽는다. 질문·답변·대화는 DB나 로그에 저장하지 않는다.

### 근거 인용 (v1부터 있는 추가 필드)

| 위치 | 필드 |
|---|---|
| `GET /api/v1/programs/{id}` | `evidence: CheckEvidence[]` (없으면 생략) |
| `GET/POST /api/v1/programs/{id}/assistant` | `eligibility.checks[].evidence: Evidence[]`, `sections[].evidence`, `sections[].source_checked_at`(공고 원문 확인일). 섹션 필드는 eligibility·documents·deadline에만 붙는다. `assistant/compare`도 같은 안내를 재사용하므로 함께 붙는다 |
| `POST /api/v1/recommend/preview` | `items[].evidence` (자격 check만, 서류 제외) |

`Evidence` 형태(`crates/core/src/models.rs`): `kind`(`official_text` · `structured_source` · `ai_extracted_approved`), `quote?`(저장 원문의 부분 문자열 그대로, 300자 초과는 `…`), `source_label`, `source_url?`(공개 https만), `captured_at?`, `approved_at?`(승인 사실만), `subject?`. `CheckEvidence`는 여기에 `check`(`age|region|enrollment|hard_rule|application_window|documents`)를 더한 것이다. AI 승인 사실이 인용되는 조건은 [mazimi-ai-extraction.md §13](mazimi-ai-extraction.md#13-승인-사실의-공개-인용)에 있다.

## 2. 근거 검색형 Q&A (`answer`)

`crates/api/src/services/grounded_qa/`. 질문마다 그 공고의 저장 자료에서 문장 단위 색인을 만들고, 찾은 문장을 그대로 인용한다.

### 문장 색인 (`passages.rs`)

요청마다 계산한다(저장하지 않음, migration 없음). 필드당 2,000자로 잘려 있어 분할·채점이 마이크로초 단위이고, 저장 색인은 수집 때마다 무효화가 필요하기 때문이다.

| 출처 | id | 섹션 |
|---|---|---|
| `summary` | `summary.<n>` | 요약 |
| `normalized_payload.conditions` | `conditions.<n>` | 지원 조건 |
| `application_method` / `submission_documents` / `screening_method` | `method.<n>` / `documents.<n>` / `screening.<n>` | 신청방법 / 제출서류 / 심사방법 |
| `raw_payload`의 허용 목록 텍스트 필드(온통청년 `sprtTrgtCn`·`addAplyQlfcCndCn`·`plcySprtCn`·`aplyYmd` 등, 정부24 `지원대상`·`선정기준`·`구비서류` 등, 복지서비스 `servDgst` 등) | `raw.<key>.<n>` | 필드별 |
| 승인된 A-09 인용 중 위 문장에 포함되지 않은 것 | `approved.<check>.<n>` | 조건·서류·기간 |
| 등록된 신청 기간·지원 금액(컬럼 값을 한 줄로) | `record.window`, `record.amount` | 기간·지원 내용 |

- 분할: 줄, 문장 끝(`.`·`!`·`?` 뒤 공백, `2026.10.01`은 유지), 글머리표(○·•·※·①…). 160자 넘는 문장은 `,`/`;`에서 약 120자 단위로 나눈다. 문장당 300자 상한.
- 연락처는 증거 원문과 같은 마스킹(`redact_bounded`)을 거친다. 허용 목록 밖 raw 키(코드·URL·연락처·카운터·새 필드)는 인용하지 않는다(명시적 스키마 결정).
- 같은 값의 필드, 같은 문장은 한 번만. 승인 인용이 기존 문장 안에 있으면 그 문장에 `approved_at`만 붙인다.
- id는 필드 텍스트가 같은 동안 안정적이다. `record.*` 문장은 원문 인용이 아니라 등록 정보이며 `kind: record`로 구분한다.

### 검색 (`text.rs`, `retrieval.rs`)

- 질문은 먼저 마스킹(`redact_question`)한다. 단어마다 조사·어미를 한 번 떼고(어간 2자 이상 유지), 의문사·동사(받을·있어·필요·가능…)와 숫자만인 단어는 버린다.
- 각 단어는 공백을 지운 문장의 **부분 문자열**로 맞춘다(`휴학`⊂`휴학생`, `서류`⊂`제출서류`, `주민등록등본`=`주민등록 등본`). 작은 동의어 묶음(마감→기간·까지, 얼마→금액·만원, 나이→연령·`<숫자>세`, 소득→분위·중위…)과 `…생`→`…`(휴학생→휴학)을 더한다.
- 점수: 문장 집합 안의 BM25(k1 1.2, b 0.75, 특정어 가중 1.0, 주제어 0.6) + 특정어 문자 바이그램 0.3 + 규칙 분류기 주제와 섹션이 맞으면 ×1.3 + 1.0. 없는 단어의 idf는 가장 드문 있는 단어와 같게 둔다.
- **찾음/못 찾음**: 주제어(서류·마감·조건·신청 등)가 아닌 **특정어**가 있으면, 그 특정어를 포함한 문장만 답이 되고 가장 좋은 문장이 특정어 가중치의 30% 이상을 덮어야 한다. 주제어만 있으면 그 단어나 주제 섹션의 문장이 답이다. 아무것도 없으면 `not_found: true`와 "공고에서 이 질문에 대한 내용을 찾지 못했어요" 안내, 화면은 공식 링크를 보여 준다.
- 상위 최대 3문장, 1위 점수의 50% 미만은 버린다. `highlights`는 원문 기준 **유니코드 코드 포인트** 오프셋(UTF-16 아님)이다.

### 판정 (`verdict.rs`)

질문 주제에 eligibility가 있을 때만 붙는다(`재학증명서 필요해?`는 서류 질문). 질문이 가리키는 조건을 고른다: 휴학·재학·졸업·수료 → 학적, 지역명 → 지역, 나이·만 N세 → 연령, 학교(캠퍼스 혜택) → 학교, 소득·분위 → 필수 조건(hard filter), 그 외 → 전체 판정.

| 필드 | 의미 |
|---|---|
| `status` | 저장 프로필 기준. `structural::structural_checks` 또는 assistant `Assessment`와 같은 값 |
| `registered` | 공고에 그 조건이 등록돼 있는지. 없으면 `unknown`(제한 없음은 통과 확인이 아님) |
| `asked_value`, `asked_status` | 질문이 말한 값(예: 휴학)을 같은 structural 함수로 등록 조건과 비교. 공고에 대한 사실이지 사용자 판정이 아니다 |
| `passage_id` | 조건을 말하는 인용 문장 |

불변식(속성 테스트 `eligibility.rs::grounded_verdicts_never_contradict_the_assessment`, 약 1만 조합): 판정은 같은 check의 eligibility 카드와 상태가 같고, 추천 목록에 보이는 공고에서 `fail`이 아니다.

### 응답 (`answer`, POST에만)

```json
{"topics":["documents"],"follow_up_topic":"documents","verdict":{...}?,
 "passages":[{"id":"documents.0","text":"신분증, 주민등록 등본","highlights":[{"start":0,"end":3}],
   "section":"documents","kind":"official_text","source_label":"온통청년 공고 원문 · 제출서류",
   "captured_at":"…","approved_at":"…"?}],
 "not_found":false,"unsupported":false,"notice":"…"}
```

- 범위 밖 질문(상태 변경·가정·계산, §3의 차단 규칙)은 `unsupported: true`, 인용 없음.
- **이어 묻기(multi-turn-lite)**: 클라이언트가 직전 `follow_up_topic`을 `previous_topic`으로 보낸다. 질문에 주제 단서가 없을 때만 그 주제를 쓴다. 서버는 대화를 저장하지 않는다.
- 기존 필드(`mode`는 항상 `rules`, `sections`, `eligibility` 등)는 그대로다. `GET`에는 `answer`가 없다.

## 3. 토픽 분류 (주제 선택)

`crates/api/src/assistant_provider/rules.rs`. 원문 부분 문자열이 아니라 단어 단위 매칭이다. 모든 질문의 주제는 이 규칙이 정한다. 모델 주제 분류(`IntentProvider`)는 `assistant-eval` 벤치마크용으로만 남아 있고 요청 경로에서 호출하지 않는다.

- 질문을 소문자화하고 공백·문장부호로 단어를 나눈다. 단서는 단어 접두 시퀀스다(`"대신 해|신청"`: 첫 요소는 단어 시작, 다음 요소는 같은 단어에 바로 이어지거나 다음 단어의 시작). `~`는 복합어 안 매칭(`~서류`가 `구비서류`에 매칭).
- `=x`는 단어 전체 일치다. 마지막 요소 `$`는 바로 뒤에 요청형이 와야 한다(`저장해 줘`, `바꿔주세요`, 문장 끝의 `바꿔`). 그래서 `저장해 둔`, `수정해야 할`, `다 체크했는데`는 변경 요청이 아니다.
- **강한 단서**가 토픽을 정한다. **약한 단서**(휴학·소득분위 같은 프로필 속성, `되나`, `필요`, `준비`, 일반 혜택 명사)는 강한 단서가 없을 때만 쓴다.
- **범위 밖 차단**은 좁은 구문만이다: `만약`, `가정하면/가정해`, `무시하고`, `이전 지시`, `관리자 비밀번호`, `다른 사용자의 …`, 변경 요청(요청형일 때만), 계산(`합쳐/총액 계산/할인 조합/같이 쓰면/금리 비교/만기 수령`), `무조건`, `확정해`. 차단은 보안 경계가 아니다. 경계는 모델·분류기에 쓰기 권한과 사실 생성 경로가 없다는 점이다.

정확도(주제 집합 완전 일치, `cargo test -p api --lib assistant_provider::rules`가 고정):

| 세트 | 기준 | 결과 |
|---|---|---|
| dev `tests/fixtures/assistant_intents.json` (100, 튜닝 대상) | ≥ 95% | 96/100 = 96.0% |
| holdout `tests/fixtures/assistant_intents_holdout.json` (61, 측정 전용, 음성 18) | ≥ 90% | 55/61 = 90.2% |
| regression `tests/fixtures/assistant_intents_regression.json` (20, 튜닝 대상) | 100% | 20/20 |

holdout 라벨은 분류기에 맞춰 고치지 않고, holdout 오답을 보고 규칙을 고치지 않는다. 다음 holdout은 실사용 로그나 사람 라벨러로 만든다.

## 4. 생성 요약 G (선택, 기본 꺼짐)

인용 답변이 먼저 즉시 나가고, 생성 요약은 **별도 요청**으로 점진적으로 채워진다. 생성은 인용 답변을 막지 않는다.

`POST /api/v1/programs/{id}/assistant/answer-stream` `{question, allow_ai_processing: true, previous_topic?}` → `application/x-ndjson`:

```
{"type":"start"}
{"type":"delta","text":"신분증과 "}        ← 작성 중(검증 전 초안)
{"type":"delta","text":"주민등록 등본을 …"}
{"type":"final","valid":true,"text":"…"}  ← 검증 통과 문장만
{"type":"final","valid":false,"text":"근거 문장을 확인해 주세요"}
```

| 거절(스트림 시작 전) | 상태 |
|---|---|
| 동의 없음 | 400 `consent_required` |
| AI 꺼짐·설정 오류·회로 차단 중 | 503 (`ai_unavailable`; local 밖에서는 5xx 본문이 정리되므로 클라이언트는 상태로 판단) |
| 서버 전체 1건 생성 중 | 429 `busy` (대기열 없음. "지금은 요약이 붐벼요") |
| 같은 사용자 10초 이내 재요청 | 429 `too_soon` |
| 분당 질문 한도 | 429 |
| 요약할 인용 문장 없음(not found·범위 밖) | 409 `no_passages` |
| 공고 없음 | 404 |

- 서버가 **다시 검색**한 문장만 모델에 보낸다(클라이언트가 보낸 문장은 받지 않는다). 입력: 마스킹한 질문, 판정 문장, 인용 문장(합계 1,800자, 약 1.5k 토큰). 프로필·서류·JWT는 보내지 않는다. 블록 태그(`<`,`>`)는 지운다.
- Ollama `/api/chat`: `stream: true`, `think: false`, `temperature: 0`, `num_predict: 160`, `num_ctx: 2048`, `options.num_thread`. 응답은 128KiB, 답변은 600자 상한. `done_reason`이 `stop`이 아니면(잘림) 실패.
- **근거 검증**(`grounded_qa/validate.rs`, 전체 문장이 끝난 뒤, 전각 문자는 ASCII로 접은 뒤): 근거(인용 문장·판정 문장·공고 제목, 사용자 질문은 제외)에 없는 숫자열(천 단위 쉼표 무시)과 비ASCII 숫자, 근거와 단위가 다른 수(`20만`→`20억`, 단위 만·억·천·원·세·살·%·개·회·명), 크기 단위가 있는 한글 수사(`삼십만`·`오만`)와 고유어 수(`서른`·`마흔`…), 근거에 없는 범위·주기어(`이상`·`이하`·`미만`·`초과`·`학기`·`연간`), 날짜어(`내일`·`다음 달`·`이달`·`상반기`·`상시`…), URL·이메일·도메인 모양, 라틴 단어와 고유명사형 단어(`…장학금`·`…지원금`·`…재단` 등), 판정과 어긋나는 자격 단정, 확신어(`무조건`·`보장돼`…), 400자·4문장 초과를 거절한다.
- 판정 대조: 검증 판정은 보고한 check·질문 값(`asked_status`)·전체 assessment가 **모두 pass일 때만 pass**, 하나라도 fail이면 fail, 그 외 unknown. pass가 아니면 "대상이에요/대상이세요/받을 수 있어요"류를, fail이 아니면 "받을 수 없어요"류를 거절한다. 같은 구문이 근거에 있으면 허용한다(공고 문장 그대로의 인용은 판정이 아니다. 대신 그 구문은 답 어디에서나 허용되는 한계가 있다).
- 거절하면 `final.valid=false`, 텍스트는 `근거 문장을 확인해 주세요`. 어떤 검사에 걸렸는지는 서버 로그에만 남기고 응답에 싣지 않는다. 생성 1건마다 회로 차단 결과를 정확히 하나 기록한다(검증 실패 = 계약 위반).
- 알려진 위험: 검증 전 초안(`delta`)이 "작성 중"으로 화면에 보인다(점진 표시 요구). 초안은 회색 기울임·점선으로 구분하고 최종 판정 전에는 결과로 쓰지 않는다.
- 동시성: 서버 전체 1건(세마포어, 대기 없음), 사용자당 10초 간격(시작과 **끝난 시점** 모두에서 다시 셈), 사용자 맵 2048. 타임아웃 `ASSISTANT_AI_TIMEOUT_SECS`(기본 120초, 1~300)는 생성 전체에 건다. 클라이언트가 끊으면 즉시 모델 요청과 슬롯을 놓고(`out.closed()`), 회로 차단에 세지 않는다.
- 회로 차단: 시간 초과·전송 오류 연속 3회 → 60초, 계약 위반(형식·잘림·근거 검증 실패) 연속 3회 → 300초. 성공 시 둘 다 초기화. `ai_available` = 설정됨 그리고 차단 중 아님.
- 공급자 이음새: `assistant_provider::AnswerProvider`(`generate(request, deltas) → full text`). Ollama만 구현돼 있다. 호스팅 공급자는 같은 trait로 추가할 수 있지만 이번에 붙이지 않았다.

### 설정

| 키 | 기본 | 의미 |
|---|---|---|
| `ASSISTANT_AI_ENABLED` | `false` | `true/1/yes/on`(대소문자 무관)만 켜짐 |
| `ASSISTANT_OLLAMA_URL` | `http://127.0.0.1:11434` | loopback·사설 IP·`ollama`만, 포트 11434 |
| `ASSISTANT_OLLAMA_ALLOW_ANY_PORT` | `false` | 11434 외 포트 허용 |
| `ASSISTANT_AI_MODEL` | `qwen3.5:9b` | 생성 모델(분류 벤치마크 기본은 `assistant-eval`의 `qwen3.5:4b`) |
| `ASSISTANT_AI_TIMEOUT_SECS` | `120` | 1~300, 생성 전체 |
| `ASSISTANT_OLLAMA_KEEP_ALIVE` | `30m` | `-1` 또는 `<n>s|m|h` |
| `ASSISTANT_AI_WARMUP` | `false` | 기동 시 빈 `/api/chat`로 모델 선로딩 |
| `ASSISTANT_AI_NUM_THREADS` | 이 프로세스가 쓸 수 있는 CPU 수(최대 64) | 1~64. Ollama `options.num_thread`. 범위 밖이면 AI 비활성(인용만) |

### 측정 속도 (Oracle 4 OCPU ARM, Ollama cpuset 3코어, `num_thread=3`)

| 모델 | 프롬프트 | 출력 | 160토큰 답변 추정 |
|---|---|---|---|
| `qwen3.5:4b` | 약 23 tok/s | 약 8.7 tok/s | 1.5k 토큰 프롬프트 약 65초 + 출력 약 18초 |
| `qwen3.5:9b` | TODO: 측정 중 | TODO: 측정 중 | TODO |

짧은 프롬프트 측정이며 `assistant-eval` 결과가 아니다. 배경: [mazimi-ai-extraction.md §10](mazimi-ai-extraction.md#ollama-스레드-설정).

### AI flag 켜는 조건 (모두 충족)

1. 사람이 검수한 Q&A 세트(§6)에서 생성 요약의 근거 검증 통과율과 사람 평가 충실도를 측정하고, 검증을 통과한 문장 중 사실 오류 0건.
2. 운영 박스에서 첫 토큰 p95와 완료 p95를 측정해 화면 문구("작성 중")와 타임아웃(120초)이 맞는지 확인, 30분 soak 동안 회로 차단 0회.
3. 같은 부하에서 비도우미 API p95 악화 **≤ 10%**, Postgres CPU 여유 유지, Ollama RSS가 메모리 한도 안.
4. 개인정보처리방침에 "질문 텍스트와 공고 문장의 자체 호스팅 모델 처리" 목적 추가, 서버 측 동의 기록(보안 L3) 구현.
5. 스토어·마케팅 문구를 출시 모드에 맞춤.

### Ollama 운영 한도

`OLLAMA_NUM_PARALLEL=1`, `OLLAMA_MAX_LOADED_MODELS=1`, `OLLAMA_DEBUG` 미설정(프롬프트 로그 방지), 로그 로테이션, 포트 비공개(내부 네트워크만), CPU 코어 제한(`cpus`/`cpuset`)과 메모리 한도로 Postgres·API 보호. API 컨테이너의 `127.0.0.1`은 호스트가 아니라 컨테이너 자신이다. Ollama는 컨테이너 cpuset을 무시하고 호스트 코어 수만큼 스레드를 띄우므로 `ASSISTANT_AI_NUM_THREADS`를 **Ollama cpuset의 CPU 수**로 명시한다.

## 5. 질문으로 혜택 찾기 (`assistant/search`)

`services/grounded_qa/intent.rs` + `services/benefit_assistant/search.rs`. 모델 없음.

- 질문을 규칙으로 `SearchIntent`로 바꾼다: 지역(`canonical_region_code`), 학적(휴학·재학·졸업·수료), 혜택 유형(장학→scholarship, 정책·지원금·수당·바우처→policy, 캠퍼스·교내→campus_benefit, 할인·구독·기업혜택→corporate_benefit), 남은 특정어 키워드(틀 단어 `청년·학생·대학생·혜택·사는`… 제외).
- 저장 프로필에서 질문의 지역·학적만 바꿔 **같은 추천 미리보기**(`recommendations::preview_on`)를 돌린다. 프로필은 바꾸지 않는다. 출생연도와 지역(질문이나 프로필)이 없으면 `required_profile_fields`를 돌려준다.
- 결과: 금융상품 제외 → 유형 필터 → 키워드가 있으면 제목·요약에 키워드(동의어 포함)가 있는 것만 → 추천 순서대로 최대 5건. 각 항목은 `RecommendationItem` + `matched_keywords`. `filter_summary`, `total_candidates`, `not_found`, `intent_empty`, `notice`(적용한 조건, 자격 확정 아님)를 함께 준다. 조건을 임의로 넓히지 않는다.
- 범위 밖 질문은 `unsupported: true`, DB 조회 없음.

## 6. 평가

- Q&A 초안 세트 `crates/api/tests/fixtures/grounded_qa_draft.json`: 시드·테스트 공고 10개의 원문을 그대로 옮기고 54문항에 기대 문장 id 또는 not-found를 붙였다. 라벨러는 **`draft-by-agent, needs human review`** 이고, 검색기를 만든 같은 세션이 라벨을 쓰고 개발 중 돌렸으므로 **측정값이 아니라 회귀 하한**이다.
- 게이트 `cargo test -p api --lib grounded_qa::eval` (2026-10-12 측정):

| 지표 | 정의 | 결과 | 게이트 |
|---|---|---|---|
| answer-passage hit@3 | 답이 있는 46문항 중 기대 id가 상위 3문장에 있는 비율 | 41/46 = 89.1% | ≥ 85% |
| not-found precision | 못 찾음이라고 한 13문항 중 실제로 답이 없는 비율 | 8/13 = 61.5% | ≥ 60% |
| not-found recall | 답이 없는 8문항 중 못 찾음이라고 한 비율 | 8/8 = 100% | 기록만 |

  놓친 5문항은 모두 표현이 다른 질문이다(원드라이브↔OneDrive, 친구한테 줘도↔코드 공유, 몇 년 무료↔1년 혜택, 회사 업무용↔상업용, 직장 다니면↔미취업). 검색기는 이때 지어내지 않고 "못 찾음"으로 물러선다. 이 5문항에 맞춘 동의어는 넣지 않았다.
- **출시 판단은 사람이 검수한 세트로 한다.** 사람 라벨러가 실제 질문(익명 로그나 사용자 인터뷰)으로 새 세트를 만들고, 초안 세트를 튜닝에 쓰지 않는다.
- 판정 일관성: §2 속성 테스트. 근거 검증: `validate.rs` 단위 테스트(새 숫자·날짜·URL·이름, 판정 모순, 충실한 바꿔 말하기 통과).

## 7. 자격 판정의 단일 원천

`mazimi_core::structural::structural_checks`가 구조 조건을 한 번만 정의한다. 추천 목록(`recommendations::evaluate`)은 이것으로 거르고, 도우미(`benefit_assistant::eligibility::assess`)와 Q&A 판정(`grounded_qa::verdict`)은 같은 결과를 설명한다.

| 조건 | 의미 |
|---|---|
| 연령 | `min_age`/`max_age` vs 출생연도, `rule_engine::age_gate`. 경계 코호트는 `unknown` |
| 지역 | 비어 있으면 전국. `["unknown"]`은 출처 범위 미확정 → `unknown`(목록에서는 숨김). 그 외 저장 지역(정규화)이 포함돼야 함 |
| 학교 | `campus_benefit`만. 저장 학교를 `resolve_university`로 카탈로그 id로 바꾼 뒤 `university_id`와 비교. 미확정은 `unknown`(목록에서는 숨김) |
| 학적 | 유형과 무관하게 `normalized_payload.enrollment_statuses`가 있으면 적용 |

목록 정책: `fail` 또는 범위 미확정은 숨긴다. 설명은 범위 미확정을 `unknown`으로 보여 주고 `fail`로 말하지 않는다. 불변식 "목록에 보이면 설명은 `fail`이 아니고, 목록의 `pass`는 설명도 `pass`"를 약 8.7만 조합 property test로 고정했다(`eligibility.rs`).

`mazimi_core::structural::exclusion`은 같은 check 목록에서 목록 제외 사유를 하나 고른다(연령 → 지역 → 학교 → 학적 순, 지역 범위 미확정은 `scope_unconfirmed`). 그다음 `hard_filter_json` 전체 트리 verdict, 랭킹 규칙, 기업혜택 미확정 규칙은 기존과 같다. `missing_fields`는 `birth_year → region_code → enrollment_status → income_bracket → 나머지` 순으로 최대 2개.

### 필터 요약 (`filter_summary`)

`POST /api/v1/recommend/preview`, `GET /api/v1/assistant/bundles`, `POST /api/v1/assistant/search`에 붙는다(프로필 미완성이면 `null`). `recommendations::preview_on`이 `current_programs` 전체를 한 엔진으로 판정하고 `FilterSummary::tally`로 센다. 필드: `considered`, `included`, `included_with_unknowns`, `excluded`, `excluded_by_reason: [{reason, label, count}]`. 한 공고는 처음 걸린 사유 하나로만 센다(`not_open` → structural → `hard_rule` / `rule_unavailable`). 불변식: `considered = included + excluded`, 사유별 합 = `excluded`. 제외된 공고 제목은 담지 않는다.

## 8. 금융상품

`program_type ∈ {financial_product, loan, finance, savings, financial}`, `source_type = fss_financial` 또는 `benefit_category = finance`는 비교(400), 묶음, 질문 검색에서 뺀다. 온라인 플랫폼의 예금·대출 비교·추천에 대한 법무 확인 전까지 유지한다. 상세·준비 화면의 정보 제공은 그대로다.

## 9. 모바일

- `app/benefit-assistant.tsx`: 질문마다 말풍선(내 질문 → 답). 답에는 판정 배지(질문 값이 있으면 "‘휴학’ 등록 조건에 없음"처럼 공고 조건 기준), 인용 문장(검색어 강조, 출처·확인일 칩), not-found 안내와 공식 공고 링크, 이어 묻기 칩. 직전 `follow_up_topic`은 기기 상태로만 들고 있다.
- 동의 체크(글리프 아닌 `accessibilityRole="checkbox"`)와 AI 사용 가능 시 인용 직후 `answer-stream`을 연다(`expo/fetch` 스트림). 초안은 "AI 요약 작성 중 · 검증 전" 점선·회색 기울임으로 따로 보이고 취소 버튼이 있다. `final.valid`일 때만 "근거 기반 요약" 배지와 함께 남고, 아니면 "근거 문장을 확인해 주세요"를 보인다.
- `app/assistant-search.tsx`: 질문 → 찾은 조건 칩, 필터 요약, 결과 카드(각 혜택에 "이 혜택에 대해 물어보기"). 진입점: 홈 배너 아래, 상황별 도우미 상단.
- 순수 함수(`lib/grounding.ts`: 코드 포인트 강조 분할, 이어 묻기 제안, 판정 배지, NDJSON 파서, 최종 문장 결정, 오류 문구)는 `npm run test:session`이 고정한다.

## 10. 남은 일

- 사람이 검수한 Q&A 세트와 그 측정값(§6). 표현이 다른 질문(동의어) 대응은 그 세트로 판단한다.
- `qwen3.5:9b` 운영 박스 측정값(§4 표의 TODO), flag-on 조건 측정.
- 보안 L3 동의 기록 테이블 (migration 필요, 다른 담당 영역과 조율).
- 여러 API 인스턴스 운영 시 사용자 한도·생성 슬롯·AI 한도의 공유 저장소(현재 프로세스 메모리).
- 브리핑의 준비 plan은 항목별로 만든다(최대 3건). 준비 서비스에 배치 API가 생기면 교체.

## 11. 코드 위치

| 책임 | 코드 |
|---|---|
| HTTP 계약·질문 한도·생성 스트림·질문 검색 | `crates/api/src/routes/assistant.rs` |
| 문장 색인·검색·판정·근거 검증·검색 의도 | `crates/api/src/services/grounded_qa/{passages,text,retrieval,verdict,validate,intent}.rs` |
| Q&A 초안 평가 | `crates/api/src/services/grounded_qa/eval.rs`, `crates/api/tests/fixtures/grounded_qa_draft.json` |
| 토픽 분류·마스킹 | `crates/api/src/assistant_provider/rules.rs` |
| 동의·동시성·회로 차단 | `crates/api/src/assistant_provider/runtime.rs` |
| Ollama 생성 스트림·분류(평가용) | `crates/api/src/assistant_provider/ollama.rs`, 전송 `crates/worker/src/ollama.rs` |
| 구조 자격 조건 | `crates/core/src/structural.rs` |
| 안내·브리핑 / 설명 / 질문 검색 | `crates/api/src/services/benefit_assistant/{mod,eligibility,search}.rs` |
| 상황별 묶음 | `crates/api/src/services/benefit_assistant/bundles.rs` |
| 비교·stack-check | `crates/api/src/services/benefit_comparison.rs`, `routes/stack_check.rs` |
| 추천 공통 평가·필터 요약 | `crates/api/src/services/recommendations.rs` |
| 근거 인용 로더 | `crates/api/src/services/evidence.rs` |
| 모바일 | `apps/mobile/app/{benefit-assistant,assistant,assistant-search,compare}.tsx`, `apps/mobile/lib/{grounding,answerStream}.ts` |
| 통합 테스트 | `crates/api/tests/assistant_cases/` (`grounded_qa.rs`) |

운영자 AI 데이터 큐레이터(A-09, 공고 원문 → 근거 달린 사실 후보 → 관리자 승인)는 사용자 질문과 분리된 별도 계약이다: [mazimi-ai-extraction.md](mazimi-ai-extraction.md). 같은 Ollama 전송 코드를 쓰지만 별도 플래그(`EXTRACTION_AI_ENABLED`, 기본 꺼짐)를 쓴다.
