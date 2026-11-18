# 수집·신청·알림 독립 감사 — 2026-10-02

범위는 워커 자동 수집, 만료 처리, 신청 기록 보존, 기관 접수 경계, 외부 전송 오류 처리다. 지역 판정/API 권한/모바일 세션의 독립 감사 결과는 별도 담당 결과와 함께 통합한다. 실제 사용자 데이터나 외부 신청을 생성하지 않았으며, 아래 DB 검증은 격리된 로컬 PostgreSQL 트랜잭션을 롤백하는 방식으로 실행했다.

## 발견 및 수정

| 심각도 | 재현 가능한 문제 | 수정·재발 방지 |
|---|---|---|
| High | 마감 알림 생성 시 `expo_token=None`, `program_id=None`을 항상 넣어 등록된 기기에 푸시가 전달되지 않고 공고 연결도 빠졌다. | 계정의 기기 토큰을 조회해 채널 설정에 맞춰 전달하고 공고 ID를 포함한다. Android 전송 필드도 `channel_id`에서 Expo 규격인 `channelId`로 수정했다. |
| High | 마감/푸시/카카오 알림 설정을 워커가 읽지 않아 사용자가 꺼도 카카오 발송을 시도할 수 있었다. | `notification_preferences`를 적용한다. 기본 카카오는 명시적 선택 전까지 꺼짐이다. 새 알림에는 `in_app_visible` 선택값도 저장해 이후 설정 변경으로 과거 푸시 전용 알림이 앱에 나타나지 않게 한다. API의 조회 제한과 함께 적용한다. |
| High | robots 일시 오류나 HTTP 429에도 수집 승인 자체를 `pending`으로 되돌려 재승인 없이는 영구 중단됐다. | 일시 장애에서는 기존 승인 기한·범위를 유지하고 페이지 요청을 중단한다. `Retry-After` 초/HTTP 날짜를 존중하며 없으면 24시간 뒤 재시도한다. 도메인 전체 대기 시간을 적용해 같은 배치의 다른 URL도 우회 요청하지 않는다. 이후에도 매번 robots를 다시 검사한다. 명시적 robots 거부/401/403은 계속 차단한다. |
| High | robots가 거부로 바뀌면 `crawl_allowed=true`와 `robots_status=blocked`가 동시에 생겨 DB 제약이 차단 증거 저장을 거부할 수 있었다. | 거부 증거와 `crawl_allowed=false`, 승인 거절 상태를 한 UPDATE에서 기록한다. robots가 다시 허용해도 권한을 자동으로 새로 부여하지 않는다. |
| High | 카카오 원문 응답·발신 키가 들어간 URL, Expo 오류 원문·토큰이 로그나 오류 저장소로 들어가는 경로가 있었다. | 원문/발신 URL/기기 토큰 로그를 제거했다. Expo 오류는 알려진 원인 코드만 남기며 응답 본문을 1MiB로 제한한다. 카카오 HTTPS·자격증명 없는 주소 검증과 리다이렉트 금지를 추가했다. 공용 오류 정리 함수는 DB URL의 사용자/비밀번호와 쿼리, 추가 운영 키도 가린다. |
| Medium | UTC 날짜로 D-1을 계산해 한국 자정 부근에서 날짜가 틀리고, 검토 보류·기간 만료 공고도 알림 대상이 될 수 있었다. | 한국 시간으로 대상 날짜·본문 날짜를 계산하고 `current_programs` 및 가장 이른 종료 시각을 사용한다. 이미 신청한 공고는 제외하고 D-1을 D-3/D-7보다 먼저 처리한다. |
| Medium | 하루 알림 제한 확인과 삽입이 분리되어 작업이 겹치면 제한을 초과할 수 있었다. | 사용자 행 잠금 아래에서 개수 확인과 삽입을 한 트랜잭션으로 처리한다. 기존 공고/날짜별 UNIQUE 제약도 유지한다. |
| Medium | 영수증이 영구 누락된 오래된 Expo 티켓 1,000개가 조회 앞자리를 계속 차지할 수 있었다. | 48시간 미확인 티켓은 `ReceiptUnavailable`로 마감한다. 수신 여부가 불명확하므로 재발송하지 않는다. 전송 응답에서 즉시 확인된 `DeviceNotRegistered`도 토큰 정리에 반영한다. |
| Medium | 새 지역 코드나 불완전한 원문이 계속 들어와도 운영자가 정상 수집으로만 볼 수 있었다. | `regions=['unknown']`인 활성 공고를 소스별 집계해 `residence_scope:{source}` 경고를 기록한다. 원문 보완/재정규화/만료로 0건이 되면 자동 해결한다. 이벤트에는 공고 본문이나 사용자 정보 없이 건수만 저장한다. |

`benefit_pages`의 비모니터링 후보는 연속 오류 5회 이후에도 7일 간격으로 제한된 재시도 기회를 갖는다. 승인 범위·유효기간·robots·SSRF·본문 크기 제한을 통과해야 하므로 일시 장애 복구가 권한 확대가 되지 않는다.

## 확인한 기존 경계

- 일일 작업은 DB에 마지막 성공·시도·임대를 기록한다. 재시작 후 누락 작업을 보충하고 실패한 소스를 다시 시도한다. 소스 키/승인이 없으면 성공으로 집계하지 않고 비활성 상태로 남긴다.
- 기간 만료는 `is_active=false`, `program_status=closed`로 처리하며 공고/신청 행을 삭제하지 않는다. `current_programs`도 조회 시점에 만료를 제외하므로 워커 지연 동안 만료 공고가 다시 노출되지 않는다.
- 신청 시점 공고와 서류 목록은 `application_program_snapshots`로 보존한다. 이미 제출한 스냅샷은 수집 갱신으로 덮어쓰지 않는다.
- 기관 접수는 기관별 권한 기한/최근 정상 연결/폼 해시/공고 revision/사용자 동의를 확인한다. 금융 계약은 직접 접수 대상에서 제외된다. 답변은 암호화하며 실제 인계 시 한 번만 제공한다.
- 접수 응답을 잃으면 `unknown`으로 두고 자동 재제출하지 않는다. 큐에서 기다리다 만료된 신청의 답변은 정리하지만 접수 이력은 남긴다. 같은 요청 ID와 진행 중 신청에 대한 중복 방지 장치가 있다.
- 페이지 수집은 승인된 호스트·경로 안에서 DNS를 공인 주소로 고정하고 리다이렉트를 무조건 따라가지 않는다. 원문 구조 변경은 의미 텍스트/해시 검사와 게시 보류로 처리한다. 변경된 혜택 조건을 LLM이 독자적으로 게시 승인하지 않는다.

## 실행한 검증

`cargo test -p mazimi-worker`를 로컬 테스트 DB 전용 실행기로 실행했다. 라이브 API 테스트 1개를 제외하고 **라이브러리 126개 + 워커 실행 파일 6개 통과**, 실패 0개다. `git diff --check`도 통과했다. 최종 `cargo check --workspace`와 `cargo test --workspace`는 통합 담당이 실행한다.

주요 새 회귀 테스트:

- 한국 자정 전후 날짜와 기기 토큰/공고 딥링크 전달.
- DB의 기본/개별 알림 설정, 마감 비활성, 검토 보류·종료 공고 제외, 앱 내 표시 선택값.
- 실제 JSON의 Android `channelId`, 오류 원문의 기기 토큰/연락처가 영수증 오류값으로 남지 않음.
- 일시 수집 장애 후 승인 유지·후속 재시도 시각, robots 거부의 DB 제약 통과, robots 허용으로 임의 재승인되지 않음.
- 지역 미확인 운영 경고의 생성→정상화 후 해결. 원본 공고 행 보존.
- DB URL 자격증명과 쿼리 토큰 가림.

기존 테스트로 robots 우선순위, 사설 IP 차단, 승인 경로 경계, HTML 차단 페이지, EUC-KR, 의미 조건 보존, API 오류 응답이 빈 성공으로 처리되지 않음을 함께 검증했다. 외부 기관 신청이나 실제 기기 푸시를 이 감사에서 발송하지 않았다.

## 남는 운영 경계

1. **권한을 새로 얻는 일은 자동화 대상이 아니다.** 기관의 신청 접수 연결 승인, 제공 API 이용 권한/키 발급·회수, 수집 금지 변경, 승인 범위를 벗어난 새 도메인은 승인된 담당자의 판단이 필요하다. 이번 수정은 이미 승인된 범위의 일시 장애를 자동 복구한다.
2. **혜택 조건이 실제로 바뀐 경우** 일반 HTML 원문의 내용을 추정해 무조건 재게시하지 않는다. 구조화된 공식/제휴 피드와 명시적 필드 계약이 사람의 반복 검수를 줄이는 기본 경로다. 승인된 원문의 단순 CSS·레이아웃 변화는 기존 의미 추출 테스트로 보호한다.
3. **알림 수신과 신청 접수 성공은 별개다.** OS 권한, 공급자 설정/쿼터, 카카오 비즈메시지 계약과 템플릿은 실제 계정·기기에서 검증해야 한다. 불명확한 외부 발송/제출을 무작정 반복하지 않는다. 감사 테스트는 실제 공급자 전달을 보증하지 않는다.
4. 이번 워커는 `in_app_visible` 열을 사용하므로 관련 마이그레이션 및 API 버전과 함께 배포해야 한다. 이전 버전으로 일부분만 되돌리면 채널 표시 규칙이 달라질 수 있다.

상태: **DONE_WITH_CONCERNS** — 재현한 코드 결함은 수정했고 로컬 회귀 검증을 통과했다. 위 권한/실제 공급자 수신 확인은 외부 운영 조건이며 성공으로 대체하지 않았다.

## 추가: 지역 조건만 빠르게 복구하는 배치 경로

저장 원문 일반 재정규화는 공고 전체를 다시 계산하며 레코드마다 여러 번 DB를 왕복한다. 지역 변경만 있는 이번 전환을 위해 기존 CLI에 `--regions-only`를 추가했다.

> **현재 상태 (2026-10-15):** 검토된 지역 전용 전환이 남아 있지 않다. local_welfare v5는 `region_districts`(시·군·구)도 바꾸므로 빠른 경로가 쓸 수 없고, `--regions-only`는 소스 지정 여부와 관계없이 "use full replay" 오류로 거부된다. local_welfare는 일반 replay를 사용한다:
>
> ```sh
> docker compose run --rm --no-deps worker renormalize-snapshots --source local_welfare --limit 100000
> ```
>
> 아래 명령은 2026-10-02 v3→v4 전환 당시의 기록이다.

```sh
docker compose run --rm --no-deps worker renormalize-snapshots --regions-only --limit 100000
# 한 소스만 처리할 수도 있다.
docker compose run --rm --no-deps worker renormalize-snapshots --regions-only --source youth_center --limit 1000
```

- 허용 전환은 `youth_center/gov_benefits/local_welfare 3→4`, `scholarship 1→2`로 명시했다. 이후 버전이 달라지면 이 경로가 자동으로 지역 변경이라고 가정하지 않는다.
- 최대 200건씩 원문 스냅샷과 공고를 함께 잠근다. 양쪽 정규화 버전이 정확히 직전 버전이고 JSON 원문이 일치하며 기존 정규화 payload가 객체인 행만 고른다. 원문 hash·JSON·버전을 쓰기 시점에도 다시 비교한다.
- 공유 지역 함수를 적용해 `programs.regions`, `normalized_payload.regions`, 두 테이블의 정규화 버전/시각만 바꾼다. 공고 `updated_at`은 갱신되지만 제목·금액·상태·활성 여부·신청 방법·수집 시각·원문·신청 스냅샷·신청 상태/이력은 그대로다.
- 공고/원문 버전 변경과 `ingestion_items` 기록은 같은 트랜잭션이다. 배치별 진행 수치도 커밋하므로 중간 실패 뒤 다시 실행하면 완료한 행은 제외된다. 다른 작업이 잡고 있는 행은 기다리며 덮어쓰지 않고 건너뛴다. 작업 종료 후 잔여 건수를 확인하고 다시 실행할 수 있다.
- 여러 버전이 밀렸거나 원문이 불일치한 행, 다른 소스는 빠른 경로에서 처리하지 않는다. 그 행에는 기존의 `--regions-only` 없는 일반 replay를 사용한다. 빠른 경로의 `selected=0`은 전체 과거 데이터 정리가 끝났다는 뜻이 아니다.

추가 DB 회귀는 원문/해시가 캡처 뒤 바뀐 경우, 프로그램 원문만 다른 경우, 두 테이블 버전이 다른 경우, 잘못된 정규화 JSON을 각각 재현해 갱신되지 않음을 확인한다. 정상 행은 종료·비활성 공고 및 신청 스냅샷/상태/이력을 유지하면서 지역만 바뀌고, 같은 전환 재실행은 0건이다. 200건 선택 제한과 201건 쓰기 요청 거부도 검증한다.

추가 검증 결과: `pipeline::region_replay::tests` 2개 통과, `cargo check -p mazimi-worker --bin renormalize-snapshots` 및 `git diff --check` 통과. 실제 프로덕션 처리 시간은 배포 후 측정해야 한다.

### 진행 중인 일반 replay를 교체하는 순서

1. 대상이 일일 수집 워커가 아닌 **해당 일회성 replay 컨테이너/프로세스**인지 확인한다. 진행 중인 run ID와 소스를 별도로 기록한다.
2. 해당 일회성 프로세스에 정상 종료 신호를 보내고 실제로 종료됐는지 확인한다. 다른 작업의 `running` 기록을 일괄 정리하지 않는다. 일반 경로는 레코드 전체가 단일 트랜잭션은 아니므로 중지 시 프로그램/원문 버전이 잠깐 다를 수 있다. 빠른 경로가 그 행을 건너뛰도록 설계했다.
3. 종료가 확인된 **정확한 run ID 하나**만 `failed`로 마감하고, 남아 있는 감사 items를 기준으로 처리 개수를 계산한다. 중지 사유를 남기고 items를 삭제하거나 성공으로 바꾸지 않는다.
4. 새 워커 이미지로 빠른 경로를 실행한다. 완료 후 미처리 버전/양쪽 버전 불일치를 확인하고 해당 소스의 일반 replay로 잔여 행을 보완한다. 전체 목록과 지역별 공개 API도 다시 확인한다.

아래 SQL은 3번의 운영 절차 예시이며 이 감사에서 프로덕션에 실행하지 않았다. 바인딩할 `$1`은 종료를 확인한 run UUID여야 한다.

```sql
WITH counts AS (
  SELECT count(*) AS selected,
         count(*) FILTER (WHERE status='upserted') AS changed,
         count(*) FILTER (WHERE status='renormalized') AS renormalized,
         count(*) FILTER (WHERE status='failed') AS failed
  FROM ingestion_items WHERE run_id=$1
)
UPDATE ingestion_runs r
SET status='failed', finished_at=now(),
    total_fetched=c.selected, total_changed=c.changed,
    total_renormalized=c.renormalized, total_failed=c.failed,
    error_message='Stopped explicitly for bounded region-only replay; committed audit items preserved'
FROM counts c
WHERE r.id=$1 AND r.status='running' AND r.run_kind='renormalization';
```
