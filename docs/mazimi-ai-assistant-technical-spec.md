# 마지미 AI 혜택 비서 기술 스펙 — [후속] 설계

> 상태: **후속(로드맵) 설계. v1 계약이 아니다.** 출시 중인 v1의 엔드포인트·수치·모듈 위치는 [v1 계약](mazimi-ai-assistant-v1-contract.md)이 유일한 기준이다.
> 2026-10-12: §3~§15의 에이전트(상태 머신, 도구 계약, 확인·멱등 실행, 대화·실행·확인 테이블과 RLS, 보존 정리, 인젝션 방어, 폴백)를 구현했다(기본 꺼짐). 실제 엔드포인트·모듈 위치(`crates/api/src/agent/`)·수치와 이 문서와 다르게 정한 점은 [에이전트 계약](mazimi-ai-agent.md) §12가 기준이다. 이 문서의 모델 설정(ctx·출력·타임아웃)과 `services/assistant/...` 배치는 그대로 적용하지 않았다.
> 작성일: 2026-10-07, 2026-10-08 v1 계약 분리
> 과거 측정: [로컬 평가 기록](validation/assistant-qwen25-local-20261007.json)은 Mac에서 `qwen2.5:7b-instruct`로 dev 세트를 잰 **과거 기록**이다. 운영 모델(`qwen3.5:4b`)·운영 박스 수치가 아니다.

## 1. [후속] 설계 목표

기존 Rust API, 규칙 엔진, PostgreSQL, 워커, Expo 앱 위에 선택적으로 동작하는 AI 오케스트레이션 계층을 추가한다. 모델은 의도를 해석하고 도구 사용을 계획하지만, 사실 조회와 상태 변경은 타입이 정의된 서비스가 수행한다.

핵심 요구사항은 다음과 같다.

- 결정론적 자격 판정과 생성형 설명의 분리
- 공식 소스에 근거한 답변
- 승인된 도구만 호출하는 최소 권한 구조
- 상태 변경 전 확인과 실행 멱등성
- AI 장애가 기존 API에 전파되지 않는 격리
- 로컬 Ollama와 외부 OpenAI 호환 API를 교체 가능한 구조
- 프롬프트·모델·도구·근거 버전의 추적 가능성

## 2. [후속] 현재 시스템과 연결 지점

| 기존 구성 | AI에서 사용하는 방식 |
|---|---|
| `mazimi_core::rule_engine` | 자격 verdict와 조건별 평가의 유일한 원천 |
| `GET /api/v1/programs` | 구조화 검색과 공식 공개 후보 조회 |
| `POST /api/v1/recommend/preview` | 프로필 기반 추천과 점수 |
| 프로그램 상세·공식 URL | 근거 Q&A의 대상 데이터 |
| 신청 준비 서비스 | 신청 체크리스트와 revision |
| 알림·북마크 API | 확인 후 실행 가능한 쓰기 도구 |
| 신청 여정·상태 API | 확인 후 실행 가능한 진행 관리 도구 |
| 운영 이벤트·소스 변경 감시 | 변경 영향 브리핑의 입력 |
| 워커 | 공고별 AI 산출물의 비동기 생성과 재검증 |

AI 계층은 기존 라우트를 HTTP로 자기 호출하지 않는다. API 내부의 서비스 함수를 도구 어댑터에서 호출해 인증, 트랜잭션, 에러 타입을 공유한다. 필요한 경우 기존 라우트의 DB 로직을 서비스 계층으로 먼저 분리한다.

## 3. [후속] 논리 아키텍처

```text
Expo / Web
    │
    ▼
Assistant Routes ── Auth / Rate limit / Idempotency
    │
    ▼
Assistant Service ── Run state machine ── Confirmation Service
    │                         │
    │                         └── Audit events
    ├── Tool Registry
    │     ├── Program Search
    │     ├── Eligibility Evaluation
    │     ├── Source Evidence
    │     ├── Preparation Plan
    │     ├── Bookmark / Alert
    │     └── Application State
    │
    ├── Grounding Builder ── PostgreSQL / source snapshots
    │
    └── ModelProvider trait
          ├── OllamaProvider ── internal Ollama
          └── OpenAICompatibleProvider ── optional fallback

Worker
    └── AI artifact jobs ── summary / change diff / extraction / evaluation
```

Ollama는 API 컨테이너 안에 설치하지 않는다. 별도 서비스로 격리하고 내부 Docker 네트워크에서만 접근한다.

## 4. [후속] 모듈 경계

권장 코드 구조는 다음과 같다.

```text
crates/api/src/
  routes/assistant.rs
  services/assistant/
    mod.rs
    service.rs
    run_state.rs
    confirmation.rs
    grounding.rs
    response_validator.rs
    tools/
      mod.rs
      programs.rs
      eligibility.rs
      sources.rs
      preparation.rs
      alerts.rs
      applications.rs
    providers/
      mod.rs
      ollama.rs
      openai_compatible.rs

crates/worker/src/
  jobs/ai_artifacts.rs

crates/core/src/
  assistant_models.rs
  assistant_policy.rs
```

`crates/core`에는 네트워크 구현을 두지 않고, 직렬화 가능한 계약·열거형·정책만 둔다. 모델 공급자 SDK 타입이 도메인 계층으로 새지 않게 한다.

## 5. [후속] 모델 선택 전략

### 5.1 후보

| 용도 | 후보 | 배포 형태 |
|---|---|---|
| 실시간 대화·도구 선택 | Qwen3.5 4B Q4 또는 9B Q4_K_M | Ollama ARM64 CPU |
| 비동기 공고 분석 | Qwen3.5 9B Q4_K_M | Ollama 작업 큐 |
| 운영 폴백 | OpenAI 호환 외부 모델 | 기능 플래그로 선택 |

Qwen3.5-9B 원본 BF16은 약 19GB이므로 현재 서버에서 사용하지 않는다. Ollama `qwen3.5:9b-q4_K_M`만 평가한다.

### 5.2 벤치마크 게이트

동일한 한국어 평가셋으로 4B와 9B를 비교한다.

- 자연어 조건 추출 정확도
- 도구명과 인자 JSON 정확도
- 규칙 verdict 보존 여부
- 근거 밖 답변 거절률
- 한국어 설명 품질
- 첫 토큰과 전체 응답 p50/p95
- 최대 RSS와 CPU 점유
- 8K 컨텍스트에서의 안정성
- 이미지 1장 처리 시간은 별도 측정

9B가 텍스트 대화 p95 30초를 넘기거나 마지미 API의 readiness·응답시간을 악화시키면 4B를 실시간 기본으로 사용하고 9B를 비동기 작업에만 사용한다.

### 5.3 런타임 기본값

- 양자화: Q4_K_M
- 컨텍스트: 8,192 토큰 상한
- 동시 생성: 1
- 로드 모델 수: 1
- 최대 출력: 일반 답변 512, 계획 답변 1,024 토큰
- 생각 모드: 기본 비활성, 복합 비교·계획에만 활성
- 모델 타임아웃: 실시간 45초, 비동기 180초
- 재시도: 형식 오류 1회, 타임아웃 재시도 없음

## 6. [후속] 요청 처리 상태 머신

```text
received
  → validating
  → planning
  → tool_running (0..N)
  → awaiting_confirmation (쓰기 작업이 있을 때)
  → executing
  → validating_response
  → completed

어느 단계에서든 → failed | timed_out | cancelled
```

한 run은 한 사용자와 한 conversation에 속한다. 동시에 같은 conversation에서 실행 가능한 active run은 하나다. 새 메시지가 도착하면 기존 run을 암묵적으로 취소하지 않고 `409 run_in_progress`를 반환하거나 큐에 넣는다.

## 7. [후속] 도구 계약

모든 도구는 고정된 이름, 버전, JSON Schema, 권한 수준, 타임아웃을 가진다. 모델이 만든 인자는 역직렬화 후 도메인 검증을 다시 통과해야 한다.

### 7.1 읽기 도구

| 도구 | 입력 | 결과 | 권한 |
|---|---|---|---|
| `program.search.v1` | 검색어, 유형, 지역, 상태, 정렬, limit | 공개 프로그램 요약 | 공개 가능 |
| `program.get.v1` | program_id | 상세·공식 출처·확인일 | 공개 가능 |
| `eligibility.evaluate.v1` | program_id, profile snapshot | verdict·조건별 trace | 로그인 선택 |
| `recommend.preview.v1` | 검증된 profile input | 추천 항목 | 공개 가능 |
| `source.evidence.v1` | program_id, 질문 범주 | 근거 구간·URL·확인일 | 공개 가능 |
| `preparation.get.v1` | program_id | revision·체크리스트 | 로그인 필수 |
| `documents.metadata.v1` | program_id | 필요 서류와 보유 여부 | 로그인 필수 |
| `application.get.v1` | program_id | 현재 신청 여정 상태 | 로그인 필수 |

### 7.2 쓰기 도구

| 도구 | 변경 | 확인 | 멱등성 |
|---|---|---|---|
| `bookmark.set.v1` | 북마크 상태 | 필수 | user + program + desired state |
| `alert.preference.set.v1` | 혜택별 알림 | 필수 | user + program + payload hash |
| `preparation.save.v1` | 완료 항목·접수 방식 | 필수 | revision 필수 |
| `application.state.propose.v1` | 신청 상태 | 필수 | 기존 상태와 전이 검증 |
| `profile.patch.v1` | 허용 프로필 필드 | 별도 확인 | version 또는 updated_at 검사 |

기관 제출, 결제, 금융 계약, 외부 메일 발송은 MVP 도구 레지스트리에 등록하지 않는다.

### 7.3 도구 결과 봉투

```json
{
  "tool": "eligibility.evaluate.v1",
  "ok": true,
  "data": {},
  "evidence": [],
  "error": null,
  "executed_at": "2026-10-07T00:00:00Z"
}
```

모델에는 내부 SQL 오류나 비밀값을 전달하지 않는다. 허용된 에러 코드와 사용자용 메시지만 전달한다.

## 8. [후속] 확인과 실행 보안

쓰기 작업은 모델 응답에서 즉시 실행하지 않는다.

1. Assistant Service가 `proposed_action`을 생성한다.
2. 서버가 도구명, 정규화된 인자, 사용자 ID, 만료시각을 직렬화해 해시한다.
3. 앱은 변경 전후 내용을 확인 카드로 표시한다.
4. 사용자가 승인하면 confirmation ID만 전송한다.
5. 서버가 사용자, 만료, 해시, 미사용 상태를 확인한다.
6. 같은 인자로 도구를 한 번 실행하고 결과를 기록한다.

확인 토큰은 10분 후 만료하고 한 번만 사용할 수 있다. 승인 이후 공고 revision이나 현재 상태가 변하면 `409 stale_confirmation`으로 거절한다.

## 9. [후속] API 초안

### 대화

```text
POST   /api/v1/assistant/conversations
GET    /api/v1/assistant/conversations
GET    /api/v1/assistant/conversations/{id}
DELETE /api/v1/assistant/conversations/{id}
POST   /api/v1/assistant/conversations/{id}/messages
GET    /api/v1/assistant/runs/{id}
POST   /api/v1/assistant/runs/{id}/cancel
```

메시지 생성은 모델 지연을 고려해 `202 Accepted`와 `run_id`를 반환한다. 모바일은 MVP에서 1초부터 최대 5초 백오프로 run 상태를 폴링한다. SSE는 폴링 안정성 검증 후 선택적으로 추가한다.

### 확인

```text
POST /api/v1/assistant/confirmations/{id}/approve
POST /api/v1/assistant/confirmations/{id}/reject
```

### 홈 브리핑

```text
GET  /api/v1/assistant/briefing
POST /api/v1/assistant/briefing/{item_id}/dismiss
```

### 공통 요청 규칙

- 인증이 필요한 경로는 기존 JWT를 사용한다.
- POST 요청은 `Idempotency-Key`를 지원한다.
- 본문 크기와 메시지 길이를 제한한다.
- 사용자별·IP별 rate limit을 분리한다.
- 클라이언트가 model, system prompt, tool name을 임의 지정할 수 없다.

## 10. [후속] 응답 스키마

모델의 자유 텍스트를 그대로 API 응답으로 사용하지 않는다. 다음 구조로 검증한다.

```json
{
  "answer": "string",
  "verdict": "pass|fail|unknown|not_evaluated",
  "citations": [
    {
      "program_id": "uuid",
      "source_url": "https://...",
      "label": "공식 공고",
      "verified_at": "timestamp",
      "claim_ids": ["deadline", "documents"]
    }
  ],
  "suggested_actions": [
    {
      "type": "open_program|prepare_application|propose_tool",
      "label": "신청 계획 만들기",
      "payload": {}
    }
  ],
  "missing_profile_fields": [],
  "warnings": []
}
```

`verdict`는 모델이 생성하지 않고 규칙 엔진 결과로 서버가 덮어쓴다. 존재하지 않는 program ID, 허용되지 않은 URL, 등록되지 않은 action type은 응답 검증에서 제거한다.

## 11. [후속] 데이터 모델 초안

최종 SQL은 구현 단계의 별도 migration으로 작성한다.

### `assistant_conversations`

- `id UUID PK`
- `user_id UUID FK`
- `title TEXT`
- `context_program_id UUID NULL`
- `created_at`, `updated_at`, `deleted_at`

### `assistant_messages`

- `id UUID PK`
- `conversation_id UUID FK`
- `role TEXT CHECK(user|assistant|tool_summary)`
- `content TEXT`
- `citations JSONB`
- `prompt_version TEXT NULL`
- `model_name TEXT NULL`
- `created_at`

도구 원본 결과와 모델 내부 추론은 메시지 본문에 저장하지 않는다.

### `assistant_runs`

- `id UUID PK`
- `conversation_id`, `user_id`
- `status`, `request_id`, `idempotency_key`
- `provider`, `model`, `prompt_version`, `toolset_version`
- `input_tokens`, `output_tokens`, `latency_ms`
- `error_code`, `created_at`, `completed_at`

### `assistant_tool_calls`

- `id UUID PK`
- `run_id`, `tool_name`, `tool_version`
- `arguments_redacted JSONB`
- `result_summary JSONB`
- `permission TEXT CHECK(read|confirm_write|forbidden)`
- `status`, `latency_ms`, `created_at`

### `assistant_confirmations`

- `id UUID PK`
- `run_id`, `user_id`, `tool_call_id`
- `payload_hash TEXT`
- `status CHECK(pending|approved|rejected|expired|executed|failed)`
- `expires_at`, `decided_at`, `executed_at`

### `program_ai_artifacts`

- `program_id`, `program_version`, `artifact_type`
- `payload JSONB`
- `source_hash`, `provider`, `model`, `prompt_version`
- `quality_status CHECK(pending|validated|rejected)`
- `created_at`, `validated_at`
- unique `(program_id, program_version, artifact_type, prompt_version)`

### `program_ai_extraction_candidates`

- `id UUID PK`
- `source_snapshot_id UUID FK`
- `program_id UUID NULL FK`
- `artifact_type`, `extractor_version`, `schema_version`
- `provider`, `model`, `prompt_version`, `source_hash`, `request_hash`
- `candidate JSONB`
- `evidence JSONB` — 필드별 `source_snapshot_id`, 원문 인용, locator
- `status CHECK(pending|validated|rejected|merged|failed)`
- `validation_errors JSONB`, `failure_count`, `retry_after`
- `created_at`, `validated_at`, `merged_at`
- unique `(source_snapshot_id, artifact_type, provider, model, prompt_version, schema_version, extractor_version)`

추출 후보는 authoritative 프로그램 필드와 분리한다. 검증 또는 관리자 승인으로 `merged` 상태가 되기 전에는 검색·추천·사용자 답변의 사실 원천으로 사용할 수 없다.

모든 사용자 테이블에 RLS를 적용하고 서비스 역할은 필요한 작업에만 사용한다. 계정 삭제 시 대화, 실행, 확인 기록의 사용자 연결 데이터를 삭제하거나 법적 보존이 필요한 최소 감사 데이터만 비식별화한다.

## 12. [후속] 검색과 근거 구성

### MVP 검색

벡터 검색을 먼저 도입하지 않는다.

1. 모델이 자연어를 검증 가능한 `SearchIntent`로 변환한다.
2. 기존 `programs.search_tsv`와 구조화 필터로 후보를 찾는다.
3. 추천 또는 상세 서비스가 현재 공개 가능한 프로그램만 반환한다.
4. 선택된 프로그램의 구조화 필드와 공식 소스 스냅샷만 grounding context로 만든다.

이 방식은 만료·비공개·미검증 공고가 임베딩 검색으로 다시 노출되는 문제를 줄인다.

### 후속 검색

원문 구간 검색 품질이 부족할 때만 `pgvector` 또는 별도 임베딩 인덱스를 도입한다. 임베딩 레코드는 프로그램 버전과 source hash에 묶고, 새 버전이 발행되면 이전 인덱스를 비활성화한다.

### 인용 규칙

- URL은 `source_policy`를 통과한 공개 HTTPS 공식 주소만 허용한다.
- 출처 라벨, 확인일, 프로그램 버전을 저장한다.
- 답변의 금액·기간·자격·서류 claim은 최소 하나의 evidence ID와 연결한다.
- 근거가 충돌하면 최신 값으로 자동 병합하지 않고 충돌을 노출한다.

## 13. [후속] 프롬프트와 인젝션 방어

프롬프트 구성 순서는 고정한다.

1. 변경 불가능한 시스템 정책
2. 도구 계약과 권한
3. 서버가 생성한 사용자·화면 맥락
4. 검색된 공식 데이터와 원문 구간
5. 사용자 메시지

공식 페이지와 사용자 입력은 모두 신뢰할 수 없는 데이터로 표시한다. 원문에 `이전 지시를 무시하라`, API 키를 출력하라, 다른 URL을 호출하라는 문장이 있어도 실행 지시로 취급하지 않는다.

도구 호출 방어:

- 모델이 URL, SQL, HTTP 메서드를 직접 지정하지 못한다.
- 도구별 allowlist 인자만 받는다.
- UUID 소유권과 공개 상태를 서버에서 다시 검증한다.
- 외부 URL fetch는 기존 소스 정책과 워커만 수행한다.
- 시스템 프롬프트와 비밀 환경변수는 모델 context에 넣지 않는다.

## 14. [후속] 개인정보와 보존

### MVP 모델 입력 허용

- 연령 또는 출생연도
- 시·도 및 필요한 경우 시·군·구 코드
- 학교명·학교 유형·학적 상태·학년
- 취업 상태
- 사용자가 입력을 허용한 소득·가구 조건
- 현재 보고 있는 프로그램과 준비 상태

### 기본 금지

- 실명, 전화번호, 이메일, 상세 주소
- 주민등록번호, 계좌번호, 인증정보
- 업로드한 서류 원본과 암호화 키
- OAuth 코드, JWT, API 키
- 기관 제출 답변 원문

민감한 자격 플래그는 해당 질문에 필요하고 사용자가 허용한 경우에만 최소 문맥으로 사용한다. 로그에는 원문 프로필 대신 필드명과 사용 여부만 남긴다.

대화 보존 기간은 출시 전에 확정한다. 사용자는 대화를 직접 삭제할 수 있어야 하고, 삭제된 대화는 모델 평가셋에 자동 편입하지 않는다.

## 15. [후속] 신뢰성·폴백

### 회로 차단기

- 연속 타임아웃 또는 오류율 기준을 넘으면 모델 호출을 일시 중지한다.
- 중지 중에는 AI 화면에 일시 사용 불가를 표시하고 기존 검색·상세·신청 준비로 연결한다.
- readiness는 DB와 핵심 API를 기준으로 유지하고 Ollama 장애 때문에 API 컨테이너를 unhealthy로 만들지 않는다.

### 캐시

- 프로그램별 요약·질문 답변 근거는 program version과 prompt version으로 캐시한다.
- 개인화된 verdict와 쓰기 제안은 공유 캐시에 저장하지 않는다.
- 공고 revision이 바뀌면 관련 artifact와 confirmation을 무효화한다.

### 재시도

- 모델 형식 오류: 더 짧은 복구 프롬프트로 1회
- 읽기 도구의 일시 DB 오류: 기존 서비스 정책 내 1회
- 쓰기 도구: 자동 재시도 금지, 멱등 키로 결과 조회
- 타임아웃: 동일 사용자 요청을 자동 재생성하지 않음

## 16. [후속] 관측성과 감사

구조화 로그에 다음을 기록한다.

- request/run ID, 익명화된 user ID
- provider, model, prompt/toolset version
- run 상태 전이와 각 단계 지연시간
- 호출한 도구명, 성공 여부, 사용자 확인 여부
- citation 수와 응답 validator 결과
- 토큰 사용량 또는 Ollama 평가 통계
- 폴백과 회로 차단기 상태

기록하지 않는 항목:

- 전체 시스템 프롬프트
- 비밀키·인증 토큰
- 서류 원문
- 모델의 chain-of-thought
- 로그 목적과 무관한 사용자 대화 전문

운영 지표는 모델 품질과 핵심 API 안정성을 분리한다. AI 지연으로 API 전체 p95가 악화되지 않도록 별도 대시보드와 알림 기준을 둔다.

## 17. [후속] Ollama 운영 스펙

현재 오라클 ARM64 서버 기준 초기 제안이다.

```text
서비스명: ollama
네트워크: Docker 내부 only
외부 포트: 없음
모델 볼륨: 전용 named volume
CPU limit: 벤치마크로 결정, 초기 3 cores 이하
Memory limit: 초기 12 GiB
Parallel requests: 1
Loaded models: 1
Context: 8192
Queue: Assistant Service에서 제한
```

필수 운영 조건:

- 모델 다운로드 후 루트 디스크 여유 8GB 이상 유지
- 모델 pull은 배포와 분리하고 checksum·모델 태그 기록
- readiness와 실제 짧은 추론 smoke test 분리
- `11434`를 Caddy 또는 공인 인터페이스에 노출하지 않음
- 컨테이너 로그 회전 설정
- 마지미 API·worker의 메모리 제한 유지
- CPU 포화 시 worker 수집 일정과 충돌하지 않게 작업 큐 조정

## 18. [후속] 워커 비동기 작업

초기 비동기 artifact:

- `program_structured_extraction_candidate_v1`
- `program_plain_summary_v1`
- `eligibility_plain_explanation_v1`
- `application_checklist_draft_v1`
- `source_change_diff_v1`
- `suggested_questions_v1`

AI 산출물은 자동으로 프로그램 원본 필드를 덮어쓰지 않는다. 구조화 추출은 후보 상태로 저장하고 기존 데이터 품질 검증 또는 관리자 승인 후 사용한다.

구조화 추출은 `규칙 파서 → LLM 보강 → JSON Schema → 도메인 검증 → 근거 검증 → 후보 저장` 순서로 수행한다. 제목, 공식 URL, 명시적인 날짜·금액처럼 결정적으로 파싱 가능한 필드는 규칙값을 우선한다. LLM은 애매한 대상 조건, 서류 분류, 예외 문장, 변경점 후보를 보강한다.

모든 후보 필드는 다음 근거를 함께 가져야 한다.

- `source_snapshot_id`
- 짧은 원문 인용 또는 정규화된 evidence text
- HTML selector, JSON path, PDF page·문단 등 재현 가능한 locator
- 추출 신뢰도와 검증 결과

근거가 없거나 원문과 모순되는 필드는 버리고, 한 필드의 실패 때문에 검증된 다른 필드까지 잃지 않도록 필드 단위 결과를 저장한다.

작업 큐는 source hash, artifact type, 모델, 프롬프트·스키마·extractor version을 멱등 키로 사용한다. 동일 조합의 성공 작업은 다시 실행하지 않는다. 실패는 지수 백오프와 쿨다운을 적용하고, 모델 장애 시 기존 규칙 정규화 결과를 유지한다.

원문을 다시 수집하지 않고 저장된 `source_snapshots`에 새 프롬프트·스키마를 적용하는 `AI_REPROCESS_ONLY` 운영 모드를 제공한다. 재처리 결과도 후보 상태로 저장하며 기존 공개 프로그램을 즉시 덮어쓰지 않는다.

## 19. [후속] 테스트 전략

### 단위 테스트

- 도구 인자 검증과 권한 정책
- confirmation hash·만료·1회 실행
- response schema validator
- URL·citation allowlist
- 개인정보 redaction
- 상태 머신의 모든 전이
- model provider timeout과 오류 매핑
- 추출 후보의 JSON Schema·enum·범위·최소/최대 역전 검증
- 원문 evidence가 없는 필드 제거와 규칙값 우선 병합
- 프롬프트·스키마·extractor version별 캐시 무효화

### 통합 테스트

- AI 없이 기존 라우트 전체 회귀
- 고정 fake provider를 사용한 대화 → 도구 → 응답
- 규칙 `Fail`을 모델이 `Pass`로 표현하려 해도 서버가 차단
- stale preparation revision에서 쓰기 거절
- 중복 approve와 idempotency 처리
- 타 사용자 conversation·confirmation 접근 차단
- Ollama 중단 시 폴백 응답과 핵심 API 정상성

### 평가셋

최소 100개 한국어 시나리오로 시작한다.

- 자연어 검색 25
- 자격 통과·실패·정보 부족 25
- 기간·금액·서류 Q&A 20
- 신청 계획 10
- 도구 호출과 확인 10
- 프롬프트 인젝션·개인정보·악성 입력 10

각 시나리오는 기대 tool call, 허용 verdict, 필수 citation, 금지 claim을 가진다. 모델이나 프롬프트가 바뀔 때 CI 또는 별도 평가 파이프라인에서 회귀를 비교한다.

## 20. [후속] 단계별 구현 순서

### Step 1. 평가 하네스

- `ModelProvider` 계약과 fake provider
- Qwen3.5 4B·9B 벤치마크 CLI
- JSON schema 검증
- 평가 데이터 포맷과 리포트
- source snapshot 기반 구조화 추출 후보 스키마와 필드별 evidence 검증
- 저장 원문 재처리와 버전 캐시 PoC

### Step 2. 읽기 전용 서비스

- conversation/run migration과 RLS
- `AssistantService`와 상태 머신
- 검색·상세·자격·근거·준비 read tools
- 비동기 message API와 폴링
- citation validator

### Step 3. 모바일 MVP

- 홈 진입 카드
- 프로그램 상세 `AI에게 물어보기`
- 대화·출처·정보 부족 UI
- 로딩, 타임아웃, 기존 기능 폴백

### Step 4. 승인형 쓰기

- confirmation migration과 API
- 북마크·알림·준비 저장 도구
- 변경 전후 확인 카드
- 감사 로그와 멱등성 테스트

### Step 5. 워커·능동 비서

- artifact 작업 큐
- 공고 변경 diff
- 홈 브리핑과 개인 영향 후보
- 관리자 품질 검수

각 step은 독립 커밋과 migration으로 구성하고, 이전 step의 기능 플래그가 꺼진 상태에서도 배포 가능해야 한다.

## 21. [후속] 환경변수 초안

```text
AI_ASSISTANT_ENABLED=false
AI_PROVIDER=ollama
AI_MODEL=qwen3.5:4b
AI_BACKGROUND_MODEL=qwen3.5:9b-q4_K_M
AI_BASE_URL=http://ollama:11434
AI_REQUEST_TIMEOUT_SECS=45
AI_BACKGROUND_TIMEOUT_SECS=180
AI_CONTEXT_TOKENS=8192
AI_MAX_OUTPUT_TOKENS=512
AI_MAX_CONCURRENT_RUNS=1
AI_PROMPT_VERSION=assistant-v1
AI_TOOLSET_VERSION=tools-v1
```

운영 환경에서 `AI_BASE_URL`은 내부 주소만 허용한다. 외부 provider를 사용할 때 API 키는 별도 비밀 저장소에서 주입하고 모바일·웹 번들에 포함하지 않는다.

## 22. [후속] 배포와 롤백

### 배포

1. migration 적용
2. AI 기능 플래그 off 상태로 API 배포
3. Ollama와 모델 준비, 내부 health 확인
4. 평가셋·smoke test 실행
5. 관리자와 내부 테스터에게만 플래그 활성화
6. 읽기 전용 트래픽 일부 활성화
7. 지표 확인 후 승인형 행동 활성화

### 롤백

- `AI_ASSISTANT_ENABLED=false`로 진입점과 모델 호출 즉시 중단
- 기존 검색·상세·신청 API는 영향 없이 유지
- pending run은 cancelled 처리
- pending confirmation은 expired 처리
- 대화 테이블은 롤백 중 삭제하지 않고 개인정보 정책에 따라 보존
- Ollama 컨테이너 중지는 DB migration 롤백과 무관

## 23. [후속] Phase 1 완료 기준

- 평가셋의 자격 verdict 불일치 0건
- 사실 claim citation 포함률 100%
- 허용되지 않은 URL·action 노출 0건
- 응답 schema 검증 성공률 99% 이상
- 타 사용자 데이터 접근 테스트 전부 통과
- Ollama 중단 중 기존 API 테스트 전부 통과
- 선택 모델이 서버 리소스와 지연시간 게이트 통과
- 모바일에서 질문, 결과, 출처, 실패 폴백 흐름 실기기 검증
- AI를 끈 상태로 운영 배포와 롤백 검증

## 24. [후속] 구현 전에 남은 결정

1. Qwen3.5 4B·9B 중 실시간 기본 모델
2. 대화 보존 기간과 사용자 삭제 정책
3. MVP에서 프로필 수정 도구 포함 여부
4. 폴링 완료 후 SSE 도입 여부
5. 외부 모델 폴백을 초기부터 둘지 여부
6. 내부 테스터와 품질 검수 책임자

위 결정 중 1번은 실제 오라클 서버 벤치마크로 확정한다. 나머지는 Phase 1 migration과 UI 구현 전에 제품·보안 관점에서 승인한다.
