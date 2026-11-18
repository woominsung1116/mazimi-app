# 마지미 AI 데이터 큐레이터 (A-09) 계약

> 상태: 코드·테스트 구현, 운영 비활성 (`EXTRACTION_AI_ENABLED`, `EXTRACTION_AUTO_APPLY_ENABLED` 모두 기본 꺼짐). 실제 모델 평가·서버 벤치마크 전.
> 범위: 공식 공고 원문에서 자격 사실 후보를 비동기로 뽑는다. 원문 근거가 있고, 관리자가 승인했거나 결정적 규칙 파서가 같은 근거에서 같은 값을 독립적으로 읽은 사실만 공개 데이터에 반영한다(§14).
> 관련: [기술 스펙 §5·§11·§18](mazimi-ai-assistant-technical-spec.md), [v1 계약](mazimi-ai-assistant-v1-contract.md)

## 1. 원칙

- **규칙 먼저, 모델은 빈칸만.** 수집 정규화가 이미 채운 필드(연령, 신청 기간·상시, 확정 지역, 학적, 기존 서류 목록)는 모델 요청 스키마에서 빠지고, 모델이 그래도 내면 버린다(`excluded_by_rules`). 모델 실패는 공개 데이터를 건드리지 않으므로 규칙 결과가 그대로 남는다.
- **모든 사실에 원문 근거.** 사실마다 저장된 원문의 정확한 인용, 문자 오프셋, 원문 해시를 갖는다. 원문에 그대로 없는 인용은 버린다. 퍼지 매칭은 하지 않는다.
- **독립 검증 없이 공개 반영 없음.** 후보는 별도 테이블에만 쌓인다. `programs`·`eligibility_rules`·`program_documents`에는 둘 중 하나로만 들어간다: 관리자가 사실별로 승인하거나, 규칙 파서가 같은 인용에서 같은 값을 읽어 자동 반영 조건(§14)을 모두 통과하거나. 두 경로는 같은 트랜잭션 코드(`apply.rs`)를 쓴다. 모델 혼자서는 아무것도 반영하지 못한다.
- **사용자 데이터는 모델에 가지 않는다.** 입력은 `programs` 행의 공개 텍스트뿐이다.

## 2. 흐름

```
수집 정규화(pipeline) ─▶ programs ─(원문 열 변경 트리거)─▶ program_ai_extraction_queue
                          │ 야간 03:30~06:30 KST 매시(하루 1회 성공, 큐 먼저) + 매시 45분 큐 소진
                          │ EXTRACTION_AI_ENABLED일 때만
                          ▼
source.rs  공개 텍스트 조립 → 연락처 마스킹 → 구분자 무력화 → 6,000자 상한 → sha256
provider.rs  Ollama /api/chat (JSON schema, temperature 0, seed 0, think off, ctx 8192, 출력 1024)
validate.rs  필드별 디코드 → 근거 앵커링 → 범위·코드·텍스트 가드 → 신뢰도 분류
crosscheck.rs  같은 인용을 규칙 파서로 다시 읽음 → agree / disagree / no_parse (auto_check)
job.rs  캐시(쿨다운) → 후보 저장(사실 단위) → 원문 변경 시 이전 후보 superseded
auto.rs  agree + pending + 허용 유형 + 스위치·상한 ─▶ apply.rs (Actor::Auto, 사실당 트랜잭션)
                          ▼
관리자 화면 /admin/ai-extraction (예외 큐·감사 표본·실행 되돌리기)
            /admin/programs/{id} ─ 사실별 승인/거절 + 메모 ─▶ apply.rs (Actor::Reviewer, 한 트랜잭션)
```

코드: `crates/worker/src/ai_extraction/{source,facts,provider,validate,crosscheck,apply,auto,job,eval}.rs`, 공유 Ollama 전송 `crates/worker/src/ollama.rs`(API 의도 분류기와 같은 허용목록), 관리자 API `crates/api/src/services/ai_extraction.rs`·`routes/ai_extraction.rs`, 화면 `apps/web/src/app/admin/ai-extraction/page.tsx`·`apps/web/src/app/admin/programs/[id]/AiExtractionReview.tsx`, 마이그레이션 `infra/migrations/20261010000001_ai_extraction.sql`·`20261012000001_ai_extraction_auto_verify.sql`.

## 3. 입력 원문

순서 고정: `[제목]`, `[요약]`, `[신청방법]`, `[제출서류]`, `[심사방법]`, 이어서 `raw_payload`의 최상위 문자열 필드(키 정렬). URL·링크·전화·메일·팩스·우편번호·주소·담당·문의 키와 `http(s)://` 값, `_`로 시작하는 내부 키는 뺀다. 같은 값은 한 번만 넣는다. 그 뒤 이메일·주민번호·전화번호·담당자 이름을 가리고(`enrichment::redact_bounded`, 기존 Gemini 경로와 같은 마스킹), `UNTRUSTED_SOURCE_TEXT` 구분자 변형을 무력화한다.

**저장되는 원문 = 모델이 본 마스킹된 텍스트**다(`program_ai_extraction_cache.source_text`). 오프셋은 이 문자열의 유니코드 문자(code point) 단위이며 끝은 미포함이다. 웹은 `Array.from(text)`로 자른다. 해시는 `sha256("program-source-v1" ‖ 0x00 ‖ text)`. 조립 방식이 바뀌면 `SOURCE_FORMAT_VERSION`을 올린다.

## 4. 출력 스키마 v1 (`SCHEMA_VERSION = 1`, `PROMPT_VERSION = "program-facts-v2"`)

모델 응답은 요청된 타입만 담은 객체다. 각 사실은 `confidence`(`HIGH|MEDIUM|LOW`)와 `evidence {quote, start}`를 갖는다.

| 타입 | 값 | 공개 반영 대상 (승인 시) |
|---|---|---|
| `age` | `min_age`, `max_age` (0~100, 만나이, **포함 경계**: `N세 미만` → `max_age = N-1`, `N세 초과` → `min_age = N+1`), `basis` `man_age|year_age|unspecified` | `programs.min_age/max_age` + 동기화 유지용 override |
| `region` | `codes` (core 지역 코드) | `eligibility_rules`에 `region_code in [...]` |
| `enrollment` | `statuses` (`enrolled|on_leave|graduated|research_student`) | `enrollment_status in [...]` |
| `employment` | `statuses` (`employed|new_hire|unemployed|preparing|self_employed|student`; `other` 불가) | `employment_status in [...]` |
| `income` | `criterion` `median_income_percent|income_decile|kosaf_bracket`, `comparator` `lte|lt|gte|gt|eq`, `value` | 소득분위 → `income_bracket`, 학자금 지원구간 → `kosaf_support_bracket` 조건. **기준 중위소득 %는 프로필 필드가 없어 기록만** |
| `document` | `name`, `required`, `notes` (최대 20개) | `program_documents` (`source='ai_extraction_approved'`, `evidence`, `ai_candidate_id`) |
| `application_window` | `kind` `dated|always_open|until_budget_exhausted`, `start_date`, `end_date` (YYYY-MM-DD, 한국 날짜) | `dated`: `application_start_at` 00:00 KST, `application_end_at` 23:59:59 KST(`pipeline::kst_start_of_day/kst_end_of_day`) + override. 상시·예산 소진은 **기록만**(상태는 수집기가 관리) |

자격 규칙은 기존 의미를 따른다: `hard_filter_json`은 명시적 모순(`fail`)만 목록에서 제외하고 정보 부족(`unknown`)은 남긴다. 승인된 조건은 `hard_filter_json`과 `rule_json`(설명·정렬) 양쪽에 같은 조건으로 들어간다. 관리자가 "자격 요건"임을 원문으로 확인했기 때문에 하드 필터로 둔다. 우대·가점 조건이면 거절한다.

## 5. 서버 검증 (모델을 믿지 않는다)

`validate.rs`의 `validate_fact`는 저장 직전과 **승인 직전(현재 원문으로 다시)** 두 번 실행된다.

1. **근거**: `source_hash`가 현재 원문 해시와 같고, `text[start..end] == quote`이며, 인용 2~300자. 모델 오프셋이 틀렸어도 인용이 원문에 **정확히 한 번**만 나오면(겹치는 출현도 셈) 서버가 위치를 잡고 `offset_source = server_anchored`로 표시한다. 여러 번 나오거나 없으면 `invalid`.
2. **범위·코드**: 나이 0~100·역전 금지, 중위소득 1~300%, 분위·지원구간 0~10, 날짜 2000~2100년·시작 ≤ 끝, core 정규 코드만, 서류명·메모 평문 길이.
3. **텍스트 가드(병합 가드)**: 인용 자체가 사실을 말해야 한다.
   - 나이: 인용에 `N세|N살|연령|나이|년생|출생`이 있고 값의 숫자가 인용에 있어야 함. `N세 미만`인데 `max_age = N`(또는 `N세 초과`인데 `min_age = N`)이면 오류, `N-1`/`N+1`만 인정. `만 N`이 없거나 `basis`가 만나이가 아니면 경고(flagged).
   - 소득: `소득공제·소득세·금융소득종합과세·소득이체·소득금액증명·원천징수`를 지운 뒤에도 `소득|중위|분위|지원구간|연봉|총급여`가 남아야 하고, 기준별 단어(`중위`/`분위`/`구간`)와 값의 숫자가 인용에 있어야 함. 비교 연산자는 인용의 단어와 맞아야 함(`lte`=이하, `lt`=미만, `gte`=이상, `gt`=초과). 하한(`gt/gte`)은 경고.
   - 지역: 코드의 한국어 이름(예: 부산, 충청북도)이 인용에 있어야 함. `광주시`(경기)는 `gwangju`가 아님. `거주|주소|소재|관내|출신|주민|지역` 같은 거주 단서가 없으면 경고(제목·기관명의 지역은 요건이 아님).
   - 학적·취업: 코드별 키워드(재학, 휴학, 졸업, 미취업, 재직 …)가 인용에 있어야 함. `student`는 `재학생·휴학생·졸업생` 속의 `학생`으로 충족되지 않음.
   - 서류: 공백을 뺀 서류명이 인용에 있어야 함.
   - 기간: 각 날짜가 인용 속 날짜(`2026.03.02`, `2026년 3월 2일`, `20260302`)여야 하고, 상시는 `상시|연중|수시`, 예산은 `예산`+`소진|조기마감`.
4. **상태**: 오류가 있으면 `invalid`(저장은 하되 승인 불가). 오류 없고 `HIGH`이며 경고가 없으면 `pending`(기본 검토 큐). 그 외는 `flagged`(승인 가능, 기본 화면에서 숨김).

한 사실의 실패는 다른 사실을 버리지 않는다. 같은 키(같은 타입, 같은 서류명)가 두 번 나오면 상태가 나은 쪽(pending > flagged > invalid)만 남긴다. 최상위가 객체가 아니거나, 알 수 없는 섹션이 있거나, 서류가 20개를 넘으면 응답 전체가 형식 오류다(provider 실패로 집계).

## 6. 저장 (마이그레이션 `20261010000001_ai_extraction.sql`)

| 테이블 | 용도 |
|---|---|
| `program_ai_extraction_runs` | 실행별 상태와 카운터: `selected, cache_hits, calls, failures, cooldown_skips, breaker_skips, budget_skips, unchanged_skips, rejected_by_guard, invalid_facts, excluded_by_rules, candidates_written, superseded` |
| `program_ai_extraction_cache` | `(program_id, source_hash, provider, model, prompt_version, schema_version)` 유일. `request_hash`, `source_text`, `rule_coverage`, `status succeeded|failed`, `response_json`, `error`, `failure_count`, `last_failed_at`, `latency_ms`, `source_type/source_id` |
| `program_ai_extraction_candidates` | 사실 1건 = 1행. `(program_id, source_hash, prompt_version, model, fact_key)` 유일(멱등). `value`, `evidence`, `confidence`, `status pending|flagged|invalid|approved|rejected|superseded`, `validation`, `reviewer_id`, `reviewed_at`, `review_note`, `applied_changes` |
| `program_ai_field_overrides` | 승인된 나이·기간 값. 수집 upsert가 매번 덮어쓰는 열이라, **같은 raw_payload(md5 일치)인 동안만** 승인 값을 유지한다. 한쪽 경계만 승인해도 쌍(min/max, 시작/끝)의 두 값을 함께 고정해 재정규화 값과 섞여 역전되지 않게 한다. 원문이 바뀌면 정규화 값으로 돌아가고 새 검토를 기다린다. 관리자가 해당 열을 직접 수정하면 같은 트랜잭션에서 override를 지운다 |
| `program_documents` (+열) | `source`, `evidence`, `ai_candidate_id` |

자동 검증(`20261012000001_ai_extraction_auto_verify.sql`)이 더한 것:

| 대상 | 내용 |
|---|---|
| `program_ai_extraction_candidates` (+열) | `auto_check`(규칙 파서 판독 `{verdict, parser_version, rule, parsed, reason, hold}`), `applied_by reviewer|auto_verified`(NULL인 승인 행 = 검토자), `auto_rule`, `auto_parser_version`, `auto_run_id`, `audit_sample`, `audit_reviewed_at/audit_reviewer_id/audit_note`, `reverted_at/reverted_by/revert_note/revert_report`. 상태에 `reverted` 추가. 자동 반영 행은 `reviewer_id IS NULL`이고 규칙·파서 버전이 반드시 있다(CHECK) |
| `program_ai_extraction_runs` (+열) | `trigger nightly|queue|manual`, `auto_apply_enabled`, `auto_verified, auto_applied, auto_held, auto_cap_skips, audit_sampled, queue_drained` |
| `program_ai_extraction_queue` | 원문 열(`title, summary, application_method, submission_documents, screening_method, raw_payload`)이 바뀌었거나 새로 들어온 활성 프로그램(기업혜택·금융상품 제외). `programs`의 INSERT/UPDATE 트리거가 넣는다. 같은 값으로 다시 쓰면 넣지 않는다 |
| `program_documents.source` | `ai_extraction_auto` 허용(자동 반영 서류). 공개 근거 인용(§13)은 출처를 구분하지 않는다 |

모든 새 테이블은 RLS `ENABLE + FORCE`, 정책은 `is_service_role()` 하나, `anon/authenticated` 권한 회수. 마이그레이션은 추가 전용이며 롤백 절차는 파일 머리말에 있다.

## 7. 워커 작업

- 일정: 03:30~06:30 KST 매시(`0 30 18-21 * * *` UTC), `daily::job` 리스(한국 날짜 기준 하루 1회 성공, 실패 시 1시간 간격 최대 3회, 30분 상한, panic 격리). 프로그램 단위로도 panic과 DB 오류를 격리해 한 건이 배치를 막지 않는다.
- 원문 변경 큐: 매시 45분(`0 45 * * * *`) `daily::run_ai_extraction_queue`가 큐의 프로그램만 최대 `EXTRACTION_AI_QUEUE_MAX_PROGRAMS`(기본 4, 0이면 끔)개, 10분 예산(15분 상한, panic 격리)으로 처리한다. 다른 실행이 35분 안에 시작돼 `running`이면 건너뛴다. 처리(캐시 적중·호출·실패·변경 없음)한 항목은 큐에서 지우고, 회로 차단으로 건너뛴 항목과 실행 중 다시 들어온 항목(더 늦은 `enqueued_at`)은 남긴다. 마스킹된 원문이 그대로면 캐시 적중이라 모델을 부르지 않는다.
- 선택: 활성, 기업혜택·금융상품 제외, 큐에 있거나 나이·기간·서류·자격 규칙 중 하나라도 빈 프로그램. 큐에 있는 것(먼저 들어온 순) → 처음 보는 것 → 현재 신청 가능 → 정책·장학·청년정책 → 오래 확인 안 한 순. 한 번에 최대 `EXTRACTION_AI_MAX_PROGRAMS`(기본 8). 다음 프로그램은 `경과 시간 + 요청 타임아웃`이 25분 예산 안일 때만 시작한다(작업 30분 상한 보호). 동시 1건, 순차.
- 원문 해시가 같고 요청 해시가 같으면 모델을 부르지 않는다(캐시 재검증만). 실패는 `EXTRACTION_AI_FAILURE_COOLDOWN_HOURS`(기본 6) 동안 재시도하지 않는다.
- 회로 차단: 같은 종류의 실패가 연속(전송 실패 3회 또는 계약 위반 응답 5회)이면 그 실행의 나머지 프로그램은 호출하지 않는다.
- 운영 이벤트: 실패가 있으면 `ai_extraction:provider_failures`(warning), 차단되면 `ai_extraction:circuit_open`(error). 실제 호출이 성공한 다음 실행에서만 해제(전부 건너뛴 실행은 해제하지 않음).
- `EXTRACTION_AI_MODE=normalize_only`: 모델을 부르지 않고 캐시된 응답을 현재 검증기로 다시 판정·저장(검증·가드 변경 후 재처리).
- 원문 해시·프롬프트·모델이 달라진 미검토 후보는 `superseded`. 같은 추출에서 **이번 요청에 포함된 타입** 중 사라진 사실도 `superseded`(서류 하나를 승인해 서류 타입이 규칙 커버로 바뀌어도 나머지 서류 후보는 열린 채 남는다). 승인·거절된 행은 바꾸지 않는다.

| 환경변수 | 기본 | 의미 |
|---|---|---|
| `EXTRACTION_AI_ENABLED` | 꺼짐 | `true/1/yes/on`만 켜짐 |
| `EXTRACTION_AI_URL` | `http://ollama:11434` | loopback·사설 IP·`ollama`, 포트 11434만 |
| `EXTRACTION_AI_ALLOW_ANY_PORT` | 꺼짐 | 11434 외 포트 허용 |
| `EXTRACTION_AI_MODEL` | `qwen3.5:9b-q4_K_M` | 기술 스펙 §5.1의 비동기 분석 모델. 벤치마크 후 확정 |
| `EXTRACTION_AI_TIMEOUT_SECS` | 180 | 5~600 |
| `EXTRACTION_AI_KEEP_ALIVE` | `5m` | 야간 일괄 후 메모리 반환 |
| `EXTRACTION_AI_MAX_PROGRAMS` | 8 | 1~200 |
| `EXTRACTION_AI_FAILURE_COOLDOWN_HOURS` | 6 | 1~168 |
| `EXTRACTION_AI_TIME_BUDGET_MINUTES` | 25 | 1~28 |
| `EXTRACTION_AI_MODE` | `extract` | `normalize_only` |
| `EXTRACTION_AI_NUM_THREADS` | 이 프로세스가 쓸 수 있는 CPU 수(최대 64) | 1~64. Ollama `options.num_thread`. Ollama cpuset의 CPU 수와 맞춘다(아래) |
| `EXTRACTION_AI_QUEUE_MAX_PROGRAMS` | 4 | 0~50. 매시 원문 변경 큐 소진 건수. 0이면 큐 소진 안 함(야간 실행이 큐를 먼저 처리) |

자동 반영 환경변수는 §14.4.

## 8. 관리자 승인 API

모두 `AdminUser`(토큰 없음 401, 관리자 아님 403). 검토자 id는 JWT에서만 읽고 본문의 알 수 없는 필드는 422.

- `GET /api/v1/admin/ai-extraction/candidates?status=pending|flagged|open|invalid|approved|rejected|superseded|reverted|auto_applied|audit&limit=` — 전체 큐. 기본 `pending`은 **예외 큐**(자동 반영되지 않은 HIGH 사실). `auto_applied`는 자동 반영된 사실, `audit`은 그중 감사 대기 표본. 항목마다 `auto_check`, `applied_by`, `auto_rule`, `auto_run_id`, `audit_sample`을 싣는다.
- 자동 반영 관련(§14.6): `GET /api/v1/admin/ai-extraction/auto-runs`, `POST /api/v1/admin/ai-extraction/candidates/{id}/audit`, `POST /api/v1/admin/ai-extraction/revert`.
- `GET /api/v1/admin/programs/{id}/ai-extraction` — 원문(`sources[].source_text`, `stale`), 후보, 사실별 `plan {changes[{target,current,proposed,overwrites}], notes, blockers}`.
- `POST /api/v1/admin/programs/{id}/ai-extraction/review` — `{decisions:[{candidate_id, decision: approve|reject, confirm_overwrite}], note}`. 메모 10~1000자 평문, 1~50건, 후보 중복 불가.

승인 규칙:
1. 한 트랜잭션. 하나라도 거부되면 아무것도 반영하지 않는다.
2. `pending|flagged`만 결정 가능(그 외 409 `candidate_not_open`).
3. 후보의 원문 해시 ≠ 현재 원문 해시 → 409 `source_changed`. 현재 원문으로 다시 검증해 실패 → 409 `evidence_invalid`.
4. 기존 값과 다르면(`overwrites`) `confirm_overwrite` 없이는 409 `overwrite_requires_confirmation`(현재·제안 값을 함께 반환). 규칙에서 "같은 값"의 단위는 필드 + 경계 방향이다: 상한(`lte/lt`)과 하한(`gte/gt`)은 공존하고, 확인 후에는 최상위 `all`의 같은 방향 조건만 교체한다(`between`은 양쪽과 겹침). 같은 필드가 `any` 분기 안에 있으면 409 `manual_edit_required`.
5. 규칙 사실은 하나의 새 `eligibility_rules` 버전으로(`created_by='ai_extraction'`, `reviewed_by`=관리자 id; 자동 반영은 `ai_extraction_auto`, `reviewed_by` NULL). `explain_json`은 공개 상세 API로 나가므로 이전 버전 값을 그대로 이어받고 출처 정보는 넣지 않는다. 후보 행의 `applied_changes`에 규칙 버전과 이전 규칙 id를 남긴다. 결과 트리는 `rule_engine::parse_rule_json` 통과 필수(실패 사유는 로그로만).
6. 나이 하한·상한, 기간 시작·끝이 서로 모순되면 400. 데이터 품질 CHECK 위반은 400 `data_quality_violation`.
7. 후보 행에 `reviewer_id`, `reviewed_at`, `review_note`, `applied_changes`(무엇을 무엇으로 바꿨는지)를 남긴다. 거절은 공개 데이터를 바꾸지 않는다.

## 9. 개인정보·보안

- 모델 입력: 공개 공고 텍스트(마스킹 후)만. 프로필·질문·서류·토큰·연락처는 경로에 없다.
- 원문은 신뢰하지 않는 데이터로 구분자 안에만 넣고, 시스템 지시는 고정 문자열이다. 모델은 URL·SQL·도구를 지정할 수 없고 출력은 스키마 + Rust 검증을 통과해야 한다.
- 엔드포인트는 사설망만(공유 허용목록), 리다이렉트·프록시 없음, 응답 64KiB 상한. 오류 문자열은 `persisted_error`로 비밀값을 지운 뒤 저장.
- 로그에는 카운터와 프로그램 id만 남긴다(프롬프트·원문 미기록). Ollama `OLLAMA_DEBUG`는 끈다.

## 10. 평가와 출시 게이트

평가 CLI(런타임 이미지 미포함, DB 불필요):

```sh
cargo run -p mazimi-worker --bin extraction-eval                       # mock 오라클: 하네스·라벨 점검
cargo run -p mazimi-worker --bin extraction-eval -- --fixture L.json --print-sources   # 라벨러용 원문
cargo run -p mazimi-worker --bin extraction-eval -- --provider ollama --model qwen3.5:9b-q4_K_M \
  --url http://127.0.0.1:11434 --fixture L.json --out report.json --gate
```

필드별 precision/recall(사실 단위, 지역·상태는 순서 무관, 서류는 공백 무관 이름), 근거 유효율(정확한 인용 / 반환 사실), 유효율, p50/p95 지연을 낸다. mock 오라클은 라벨을 그대로 되돌려 주므로 **품질 측정이 아니다**. 저장소의 SAMPLE(`crates/worker/tests/fixtures/ai_extraction_sample.json`, 12건)은 에이전트 초안이며 `human_verified=false`다. 라벨 형식은 [validation/ai-extraction/README.md](validation/ai-extraction/README.md).

`EXTRACTION_AI_ENABLED=true` 조건(모두 충족):
1. 사람이 검증한 100건 라벨셋(`export-label-candidates.sql`로 출처·유형 층화 추출, 2인 검토 후 `human_verified=true`)에서 운영 모델로 `--gate` 통과: 전체 precision ≥ 0.95, recall ≥ 0.60, 근거 유효율 ≥ 0.98, provider 실패 0.
2. Oracle 서버 벤치마크(`scripts/ai/benchmark-ollama.sh`): 프로그램당 p95 ≤ 180초, Ollama 메모리 한도(기본 8GiB) 미만, 실행 내내 Besu·Postgres·API 정상, 2코어(`cpuset`) 제한에서 API p95 악화 ≤ 10%.
3. 검토자 1인 이상이 관리자 화면에서 승인·거절·덮어쓰기 확인 흐름을 실제 데이터로 1회 수행.

`EXTRACTION_AUTO_APPLY_ENABLED=true` 조건은 §14.5(자동 반영 게이트).

Ollama 운영 한도는 `infra/docker/compose.ollama.yml`(선택 overlay, `ai` 프로필, 포트 비공개, 워커와만 공유하는 `ai` 네트워크(Caddy·API·웹은 접근 불가), `OLLAMA_NUM_PARALLEL=1`, `OLLAMA_MAX_LOADED_MODELS=1`, 버전 고정 필수).

### Ollama 스레드 설정

Ollama는 컨테이너 cpuset을 무시하고 **호스트** 코어 수만큼 스레드를 띄운다. 4 OCPU 호스트(aarch64 Neoverse-N1)에서 cpuset 2~3코어로 묶으면 스레드가 남는 코어를 두고 경합해 생성이 약 45배 느려졌고, 이 때문에 첫 벤치마크의 12개 프로그램이 모두 180초 타임아웃에 걸렸다.

| 모델 | `num_thread` 미지정 | `num_thread=3` |
|---|---|---|
| `qwen3.5:4b` | 0.19 tok/s | 8.7 tok/s |
| `qwen2.5:3b` | 0.25 tok/s | 12.9 tok/s |

이 수치는 짧은 프롬프트 1회 측정이며 평가(`extraction-eval`) 결과가 아니다.

그래서 우리가 보내는 모든 Ollama 요청(워커 추출, API 도우미의 분류와 warm-up, `extraction-eval`)은 `options.num_thread`를 싣는다. 값은 `EXTRACTION_AI_NUM_THREADS` / `ASSISTANT_AI_NUM_THREADS` / `--num-thread`(1~64)이고, 미설정이면 요청을 보내는 프로세스가 쓸 수 있는 CPU 수(cgroup·cpuset 반영)다. Ollama가 다른 컨테이너에서 더 작은 cpuset으로 돌면 이 기본값이 틀리므로 운영에서는 **Ollama cpuset의 CPU 수로 명시**한다(compose 기본 `OLLAMA_CPUSET=2,3` → 2, 전용 호스트에서 `1,2,3` → 3). `benchmark-ollama.sh`는 `CPUSET`에서 `NUM_THREADS`를 계산해 넘긴다.

## 11. 롤백

`EXTRACTION_AI_ENABLED` 제거 → 워커 재시작. 후보 생성만 멈추고 공개 데이터는 그대로다. 잘못 승인된 사실은 관리자 화면·API로 해당 값을 직접 수정한다(나이·기간 수정 시 override 자동 삭제, 규칙은 새 버전). 잘못 **자동 반영**된 사실은 §14.6 되돌리기 절차를 쓴다. 테이블 제거 절차는 마이그레이션 머리말 참고(자동 실행 금지).

## 12. 남은 결정

- 근거 위치 재앵커링: 모델 오프셋이 틀려도 유일한 인용이면 서버가 위치를 잡는다. 엄격 모드(모델 오프셋만 인정)로 바꿀지는 실제 모델 평가에서 `server_anchored` 비율을 보고 정한다.
- 기준 중위소득 %: 프로필에 해당 필드가 없어 규칙으로 만들지 않는다. 프로필 확장 여부는 제품 결정.
- 상시·예산 소진: `program_status`를 수집기가 관리하므로 기록만 한다. 승인된 날짜도 `program_status`를 다시 계산하지 않는다(수집 upsert는 정규화 날짜로 상태를 계산). 공개 목록의 "지금 신청 가능" 판정은 `currently_available_sql`이 날짜 열(승인 값 포함)을 직접 보므로 노출은 맞지만, 상태 배지는 다를 수 있다. 상태 재계산을 승인·upsert 양쪽에 넣을지 결정 필요.
- 지역 사실은 규칙으로 반영되며, 출처 범위가 `["unknown"]`인 공고의 목록 숨김은 그대로다(구조 지역 판정은 수집기 소관).
- 승인 사실 인용의 읽기 비용: 승인 사실이 있는 프로그램은 요청마다 `build_source`(조립·마스킹·sha256)를 다시 계산한다. 승인 건수가 늘면 원문 변경 시 approved 후보도 DB에서 표시하는 방식을 검토한다.

## 13. 승인 사실의 공개 인용

승인된 사실은 공개 API의 `evidence`(형태: [v1 계약 §1 근거 인용](mazimi-ai-assistant-v1-contract.md#근거-인용-추가-필드))에 `kind = ai_extracted_approved`로 나간다. 읽을 때마다 `crates/api/src/services/evidence.rs`의 `load`가 아래를 모두 확인하고, 하나라도 어긋나면 인용하지 않는다(fail closed).

1. `status = 'approved'`만. pending·flagged·invalid·rejected·superseded는 인용하지 않는다.
2. 프로그램의 **현재** 원문을 `build_source`로 다시 만든 해시가 후보의 `source_hash`와 같다. 공고가 바뀌면 읽는 시점에 인용이 사라진다. 승인으로 반영된 값은 그대로 남는다.
3. 현재 원문의 `[start..end]`가 저장된 quote와 정확히 같다.
4. 승인 값이 지금도 공개 값이다.

| 사실 | 공개 값 확인 |
|---|---|
| 나이 | `min_age`/`max_age` 열이 승인 값과 같다 |
| 날짜 기간 | `application_start_at`/`application_end_at`이 승인 날짜의 KST 경계(00:00 / 23:59:59)와 같다 |
| 상시·예산 소진 | 날짜 기간이 저장돼 있지 않은 동안만 |
| 지역·학적·고용·소득 | 최신 규칙 버전의 `hard_filter_json`에 같은 조건이 **필수 조건**으로 있다: 루트 leaf이거나 중첩 `all` 안. `any` 분기 안의 조건은 선택지일 뿐이라 인용하지 않는다 |
| 서류 | `program_documents`에 같은 `document_key`의 서류가 있다. `subject`에 그 서류명을 넣는다 |

- 같은 사실(`fact_key`)에 승인 행이 여럿이면 `reviewed_at`이 가장 최근인 유효한 행 하나만 인용한다.
- 출처 라벨은 `<출처> 공고 원문 · <섹션>`이다(예: `온통청년 공고 원문 · 요약`). 섹션은 quote가 놓인 원문 헤더(제목·요약·신청방법·제출서류·심사방법)에서 가져오고, `raw_payload` 키 영역이면 섹션을 붙이지 않는다. 수집 출처가 없으면 `마지미에 등록된 공고 내용`.
- "AI 판단"이라는 표현은 쓰지 않는다. 근거는 공고 원문이고, 승인은 `approved_at`으로만 드러난다.
- 기준 중위소득 % 사실은 규칙으로 컴파일되지 않으므로 인용 대상이 아니다.
- 자동 반영된 사실(`applied_by='auto_verified'`)도 `status='approved'`라 같은 조건으로 인용된다. 되돌린 사실(`reverted`)은 인용하지 않는다.

## 14. 자동 검증과 자동 반영

사람은 예외만 검토한다. 결정적 규칙이 모델과 **독립적으로** 같은 값을 읽은 사실은 사람 승인 없이 반영할 수 있다. 기본은 꺼짐이며, §14.5 게이트를 통과한 유형만 켠다.

### 14.1 규칙 교차 확인 (`crosscheck.rs`, `PARSER_VERSION = "crosscheck-v2"`)

후보를 저장할 때마다 사실의 인용(`evidence.quote`)을 유형별 규칙 파서로 다시 읽어 `auto_check`에 저장한다. 판정은 `agree`(같은 값), `disagree`(다른 값, `reason`에 규칙 판독과 모델 값), `no_parse`(파서가 받지 않는 문장, `reason`에 이유)다. 파서는 일부러 좁다. 모호하면 추측하지 않고 `no_parse`로 사람에게 넘긴다.

| 유형 | 규칙(`rule`) | 읽는 것 | 받지 않는 것(→ no_parse) |
|---|---|---|---|
| 나이 | `age_range`, `age_upper`, `age_lower`, `age_bounds` | `만 19세 이상 34세 이하`, `19~34세`, `만 19세부터 34세까지`, `만 39세 이하/까지`, `39세 미만`(→38), `18세 초과`(→19) | 인용 안의 다른 `N세/N살`, 범위 두 개, 출생연도·`연 나이`, 역전·0~100 밖, 더 긴 숫자의 일부(`2026~34세`) |
| 신청 기간 | `window_range`, `window_start`, `window_end`, `window_always_open`, `window_budget` | `2026.03.02 ~ 2026.04.30`, `2026년 3월 2일부터 4월 30일까지`(연도 생략 끝 날짜는 앞 날짜 연도, 앞서면 다음 해), `20260302~20260430`, `…까지`/`…부터`, `상시`·`연중`, `예산`+`소진/조기마감` | 시각(`18:00`, `18시까지`): 반영 열은 KST 하루 경계라 의미가 바뀜. 날짜 3개 이상(차수), 날짜와 상시 혼재, 상시와 예산 소진 동시, 구분자 없는 두 날짜, 존재하지 않는 날짜, `수시` |
| 서류 | `document_vocabulary` | 큐레이션 어휘(`DOCUMENT_VOCABULARY`: 재학증명서, 주민등록등본, 가족관계증명서, 소득금액증명원, 건강보험 자격득실확인서 등 32종과 별칭)를 최장 일치로 읽음(`사업자등록증명원`≠`사업자등록증`). 이름 뒤 `해당자/해당 시/선택/필요 시/있는 경우` → 선택 서류 | 어휘 밖 이름, 일반어(`등본`, `증명서`), `제외/불필요/생략`. `required`가 다르거나 `notes`가 인용에 그대로 없으면 disagree |
| 지역 | `region_cue` | 시·도 이름(정식·약칭)을 core 지역 코드로, 거주 단서(`거주/주소/주민등록/소재/전입`) 필수 | `광주시`(경기), 대학 이름(`부산대`), `제외/타 지역` |
| 학적·취업 | `enrollment_cue`, `employment_cue` | `재학/휴학/졸업/연구생`(`졸업예정`→enrolled), `미취업/구직/재직/자영업/프리랜서/학생`(재학생 속 `학생` 제외) → core 코드 | 부정·예외 단어, 서류 이름 속 단어(`재학증명서`) |
| 소득 | `income_median`, `income_decile`, `income_kosaf` | `기준 중위소득 N% 이하/미만/이내`, `소득분위 N분위 이하`, `학자금 지원구간 N구간 이하` | 기준 두 개 이상, 기준 단어 없는 소득 문장 |

파서의 받는 언어가 바뀌면 `PARSER_VERSION`을 올린다. 버전이 다른 `agree`는 자동 반영되지 않는다(다음 실행에서 다시 판정).

### 14.2 자동 반영 조건 (모두 충족)

1. 근거가 저장 원문의 해당 오프셋에 정확히 있다(§5.1).
2. 상태가 `pending`이다: 신뢰도 HIGH, 검증 오류·경고 없음(병합 가드 포함).
3. `auto_check.verdict = agree`이고 현재 `PARSER_VERSION`이다.
4. 반영 계획이 기존 값을 덮어쓰지 않는다. 정규화가 채운 값, 관리자가 넣은 값, 이전 승인 값이 다르면 `overwrite_requires_confirmation`으로 보류한다. 자동 반영은 `confirm_overwrite`를 할 수 없다. 규칙이 `any` 분기 안에 있으면 `manual_edit_required`.
5. 같은 원문의 같은 사실을 사람이 거절했거나 자동 반영을 되돌린 적이 없다(`human_decision_exists`).
6. 유형이 `EXTRACTION_AUTO_APPLY_TYPES`에 있고, `EXTRACTION_AUTO_APPLY_ENABLED`가 켜져 있고, 이번 실행의 상한 안이다.

반영은 관리자 승인과 **같은 함수**(`apply::apply`)를 `Actor::Auto`로, 사실 하나당 한 트랜잭션으로 부른다. 그 안에서 잠근 행으로 2·3·5를 다시 확인하고, 현재 원문으로 근거를 다시 검증한다. 하나라도 어긋나면 반영하지 않고 `auto_check.hold`에 이유를 남긴다(`auto_apply_disabled`, `type_not_enabled`, `run_cap_reached`, 위 충돌 코드, `apply_error`). 보류·불일치 사실은 `pending`으로 남아 예외 큐에 나온다.

### 14.3 출처 기록

| 위치 | 사람 승인 | 자동 반영 |
|---|---|---|
| 후보 행 | `applied_by='reviewer'`, `reviewer_id`, 메모 | `applied_by='auto_verified'`, `reviewer_id` NULL, `auto_rule`, `auto_parser_version`, `auto_run_id`, `audit_sample`, 메모 `자동 검증 반영: 규칙 … (crosscheck-v2), 실행 …` |
| `program_documents.source` | `ai_extraction_approved` | `ai_extraction_auto` |
| `eligibility_rules` | `created_by='ai_extraction'`, `reviewed_by`=관리자 | `created_by='ai_extraction_auto'`, `reviewed_by` NULL |
| `program_ai_field_overrides.approved_by` | 관리자 | NULL |
| 화면 | 승인됨 | `자동` 표시, 규칙 판독 한 줄 |

### 14.4 환경변수

| 환경변수 | 기본 | 의미 |
|---|---|---|
| `EXTRACTION_AUTO_APPLY_ENABLED` | 꺼짐 | 전역 스위치. 끄면 교차 확인은 계속 기록하되 아무것도 반영하지 않는다(`hold=auto_apply_disabled`) |
| `EXTRACTION_AUTO_APPLY_TYPES` | `age,application_window,document` | 자동 반영 유형(쉼표). 지역·학적·취업·소득은 라벨 평가가 게이트를 통과할 때까지 사람 검토. 알 수 없는 이름은 시작 오류 |
| `EXTRACTION_AUTO_APPLY_MAX_PER_RUN` | 25 | 1~500. 한 실행의 자동 반영 상한. 넘으면 `run_cap_reached`로 보류하고 운영 이벤트 `ai_extraction:auto_apply_spike`(warning)를 연다. 상한 안에서 끝난 다음 자동 반영 실행이 해제한다 |
| `EXTRACTION_AUTO_AUDIT_PERCENT` | 10 | 0~100. 자동 반영 사실 중 사후 사람 확인 표본 비율 |
| `EXTRACTION_AUTO_AUDIT_SEED` | `mazimi-auto-audit-v1` | 표본 시드. 표본 = `sha256(seed ‖ 0x00 ‖ 후보 id)` 앞 8바이트 mod 100 < 비율. 같은 시드·후보면 항상 같은 결과라 재현할 수 있다 |

### 14.5 자동 반영 게이트

유형을 `EXTRACTION_AUTO_APPLY_TYPES`에 넣거나 `EXTRACTION_AUTO_APPLY_ENABLED`를 켜기 전에(모두 충족):

1. §10의 1번(전체 추출 게이트)을 운영 모델로 통과한다.
2. 같은 사람 검증 라벨셋(`human_verified=true`)에서 자동 검증 부분집합 게이트를 통과한다:

   ```sh
   cargo run -p mazimi-worker --bin extraction-eval -- --provider ollama --model <운영 모델> \
     --url http://127.0.0.1:11434 --fixture L.json --out report.json --auto-gate \
     --auto-types age,application_window,document
   ```

   리포트의 `auto_verified`는 유형별로 "pending + 규칙 agree" 사실만 라벨과 비교한다(서류는 `required`까지 일치해야 정답). 유형마다 **precision ≥ 0.98, 지원 건수 ≥ 20**이어야 한다(`--min-auto-precision`, `--min-auto-support`). `recall` 열은 라벨 사실 중 사람 없이 반영될 비율이다. mock 오라클·사람 미검증 라벨에서는 게이트가 적용되지 않는다(종료 코드 2).
3. 켠 뒤 첫 주는 `EXTRACTION_AUTO_AUDIT_PERCENT`를 높게(예: 30) 두고 감사 표본에서 오류가 0건인지 본다. 오류가 나오면 §14.6으로 되돌리고 해당 유형을 뺀다.

지역·학적·취업·소득은 같은 절차로 각자 게이트를 통과해야 목록에 넣는다.

### 14.6 감사와 되돌리기

- 화면: `/admin/ai-extraction`. 기본은 예외 큐(`pending`). `감사 표본` 탭에서 사실별로 `감사 확인`(메모 필수) 또는 `이 사실 되돌리기`. `자동 반영 실행` 목록에서 실행 단위로 `실행 되돌리기` → 건수 미리보기(dry run) → `확정`.
- API(모두 `AdminUser`, 메모 10~1000자 평문, 알 수 없는 필드 422):
  - `GET /api/v1/admin/ai-extraction/auto-runs` — 자동 반영이 있었던 최근 50개 실행과 유지·취소·감사 대기 건수.
  - `POST /api/v1/admin/ai-extraction/candidates/{id}/audit` `{note}` — 감사 표본 확인. 감사 대기 표본이 아니면 409.
  - `POST /api/v1/admin/ai-extraction/revert` `{run_id | parser_version [+ rule] | candidate_id, note, dry_run?}` — 셋 중 정확히 하나. `parser_version`·`rule`은 `[A-Za-z0-9_.-]` 1~100자. 응답 `{matched, reverted[], skipped[{candidate_id, reason}], kept[{candidate_id, target, reason}], dry_run}`.
- 되돌리기 동작(`apply::revert`): `applied_by='auto_verified'`이고 `approved`인 행만, 최근 것부터, 사실당 한 트랜잭션.
  - `programs` 나이·기간 열은 **지금도 자동 반영 값일 때만** 이전 값(`applied_changes.changes[].current`)으로 돌린다. 그 사이 바뀐 열은 그대로 두고 `kept`로 알린다.
  - 그 사실이 쓴 `program_ai_field_overrides`와 `program_documents`(`source='ai_extraction_auto'`)를 지운다. 사람이 넣은 서류는 건드리지 않는다.
  - 규칙 사실은 자동 반영이 만든 버전이 아직 최신일 때만 그 버전을 지운다(이전 버전이 다시 최신). 이후 버전이 있으면 `skipped`(수동 수정).
  - 후보는 `reverted`(`reverted_by`, `revert_note`, `revert_report`)가 되고 다시 자동 반영되지 않는다. 같은 원문의 같은 사실은 다른 모델이 내도 `human_decision_exists`로 보류된다.
- 운영 절차(파서 결함 의심 시):
  1. `EXTRACTION_AUTO_APPLY_ENABLED=false`로 워커를 재시작한다(이후 반영 중단, 교차 확인은 계속 기록).
  2. `POST /revert {"parser_version":"crosscheck-v2","rule":"<규칙>","note":"…","dry_run":true}`로 범위를 확인하고, `dry_run` 없이 다시 보낸다. 특정 실행이면 `run_id`.
  3. `skipped`·`kept` 항목은 프로그램 화면에서 직접 고친다.
  4. 파서를 고치고 `PARSER_VERSION`을 올린 뒤 §14.5 게이트를 다시 통과하면 다시 켠다.

### 14.7 원문 변경 트리거

수집 upsert(또는 관리자 수정)가 원문 열을 바꾸면 `programs` 트리거가 `program_ai_extraction_queue`에 넣는다(`created`/`source_changed`). 야간 실행은 큐를 먼저 처리하고, 매시 45분 큐 소진(§7)이 같은 날 처리한다. 실제 증거 원문 해시는 워커가 계산하므로, 마스킹 후 같은 원문(예: `_` 내부 키만 바뀜)이면 캐시 적중으로 모델을 부르지 않는다. 원문이 바뀌면 이전 미검토 후보는 `superseded`, 새 후보는 다시 교차 확인을 거친다. 이미 반영된 값은 §6 override 규칙대로 같은 raw_payload 동안만 유지된다.
