# Mazimi Oracle Cloud 운영 런북

이 문서는 Oracle Cloud VM으로 API·웹·worker를 옮길 때의 실행 순서와
롤백 기준을 고정한다. 현재 권장 1단계는 **컴퓨팅만 Oracle로 옮기고
PostgreSQL은 Supabase에 유지**하는 것이다. DB까지 VM으로 옮기는 작업은
백업·복구 훈련을 통과한 뒤 별도 컷오버로 수행한다.

## 1. 목표 구성

```text
Internet → OCI NSG (22/80/443만) → Caddy → Web / API
                                      └── Worker (외부 포트 없음)
API / Worker ── TLS ── Supabase PostgreSQL
백업 ──────────────── OCI Object Storage 또는 별도 S3 저장소
```

OCI는 서브넷 전체보다 VNIC 단위로 적용하기 쉬운 Network Security Group을
권장한다. VM에는 22번을 관리자 IP에만, 80/443만 전체에 공개한다. 5432는
Compose에서 호스트 포트로 게시하지 않으며 NSG에도 열지 않는다.
[OCI 네트워크 보안 공식 문서](https://docs.oracle.com/en-us/iaas/Content/Network/Concepts/securitylists.htm)

## 2. VM 준비

1. Ubuntu 24.04 LTS 플랫폼 이미지를 사용한다. Ubuntu 이미지의 기본 SSH
   사용자는 `ubuntu`다.
   [OCI 플랫폼 이미지 공식 문서](https://docs.oracle.com/en-us/iaas/Content/Compute/References/images.htm)
2. 운영용 SSH 키만 등록하고 비밀번호 로그인을 사용하지 않는다.
3. 저장공간은 Docker 이미지, 로그, 백업 증가분을 포함해 잡는다. DB를 VM에
   둘 경우 별도 Block Volume과 OCI 볼륨 백업 정책을 추가한다.
4. 저장소를 받은 뒤 호스트 부트스트랩을 실행한다.

```bash
sudo bash scripts/oracle-bootstrap.sh
# docker 그룹 반영을 위해 SSH 재접속
```

스크립트는 편의 설치 스크립트가 아니라 Docker의 서명된 apt 저장소를 사용한다.
Ubuntu 24.04와 ARM64/x86_64는 Docker Engine의 공식 지원 대상이다.
[Docker Ubuntu 설치 공식 문서](https://docs.docker.com/engine/install/ubuntu/)

## 3. 1단계: Supabase DB 유지

`/opt/mazimi/.env.production`에서 다음을 설정한다.

```dotenv
DATABASE_MODE=external
DATABASE_URL=postgresql://postgres.<project-ref>:<URL_ENCODED_PASSWORD>@<session-pooler>:5432/postgres?sslmode=require
BENEFIT_CRAWLER_ENABLED=false
BENEFIT_LLM_ENABLED=false
PRE_DEPLOY_BACKUP=true
```

- 비밀번호의 `@`, `/`, `:`, `#`, `%`는 DSN 안에서 percent-encoding 한다.
- SQLx 장기 실행 서비스는 6543 transaction pooler가 아닌 direct/session
  연결의 5432를 사용한다.
- crawler와 LLM은 키가 존재해도 위 플래그가 `false`면 실행되지 않는다.
- 직접 수집은 관리자 승인, 허용 경로, 이용조건 검토, robots 사전검사가 모두
  유효한 도메인에만 별도로 열린다.

최초 배포:

```bash
cd /opt/mazimi
chmod +x scripts/*.sh
./scripts/deploy.sh
docker compose ps
curl -fsS "https://${DOMAIN}/api/health"
```

`deploy.sh`는 마이그레이션 직전 checksum 포함 백업을 만들고, migration
실패 시 애플리케이션 기동을 중단한다.

## 4. 운영 검증

DNS를 바꾸기 전에 임시 hosts 레코드나 OCI IP로 다음을 확인한다.

- `docker compose ps`: api, web, worker, caddy가 healthy/running
- `/api/ready`: DB 연결과 스키마 버전이 모두 일치하며 200
- `/admin/operations`: 미해결 운영 이벤트와 데이터 품질 이슈 확인
- `/admin/sources`: 변경·접근불가·재검증 기한 큐 확인
- 추천 미리보기: 하드 필터 Fail 제외, 정보 미입력 Unknown 유지
- `docker compose logs --tail=200 worker`: 예약 작업 panic/DB 오류 없음
- `docker stats`: 지속적인 OOM 또는 디스크 증가 없음

worker 이미지에는 다음 운영용 1회 실행 명령도 포함된다.

```bash
docker compose run --rm --no-deps worker seed-official-candidates
docker compose run --rm --no-deps worker revalidate-benefits
docker compose run --rm --no-deps worker renormalize-snapshots --limit 1000
docker compose run --rm --no-deps worker enrich-benefit-candidates
```

장기 실행 worker를 운영하지 않거나, 플랫폼에서 컨테이너를 주기적으로
기동하는 방식이 필요하면 systemd timer로 동일한 1회 작업을 등록할 수 있다.
저장소의 `scripts/systemd/mazimi-revalidate.{service,timer}`를 설치한 뒤
활성화한다. `Persistent=true`이므로 VM이 예약 시각에 꺼져 있었으면 부팅 후
한 번 보충 실행한다.

```bash
sudo install -m 0644 scripts/systemd/mazimi-revalidate.service /etc/systemd/system/
sudo install -m 0644 scripts/systemd/mazimi-revalidate.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now mazimi-revalidate.timer
systemctl list-timers mazimi-revalidate.timer
sudo journalctl -u mazimi-revalidate.service -n 100 --no-pager
```

기본 Compose 구성에서는 `worker` 서비스가 singleton lease를 획득하고 내부
스케줄을 실행하므로 timer와 동시에 활성화하지 않는다. 둘을 함께 사용할
경우 중복 실행은 lease로 차단되지만, 운영에서는 한 가지 방식만 선택한다.

마지막 명령은 `BENEFIT_LLM_ENABLED=true`가 아니면 실행되지 않으며, 실행돼도
관리자 초안만 만들고 프로그램을 자동 공개하지 않는다.

## 5. 알림과 백업

운영 이벤트는 웹훅과 무관하게 PostgreSQL에 남는다. Slack을 예로 들면 다음만
추가한다.

```dotenv
OPS_ALERT_WEBHOOK_URL=https://hooks.slack.com/...
OPS_ALERT_WEBHOOK_FORMAT=slack
OPS_ALERT_MIN_SEVERITY=warning
OPS_ALERT_DASHBOARD_URL=https://mazimi.kr/admin/operations
```

VM 내부 백업만으로는 VM 손실을 복구할 수 없다. `USE_S3=true`와 AWS CLI를
설정해 별도 저장소로 전송한다. OCI Object Storage는 S3 호환 API와 기존 S3
도구를 지원하므로 customer secret key와 전용 최소권한 정책을 사용한다.
[OCI Object Storage S3 호환 공식 문서](https://docs.oracle.com/en-us/iaas/Content/Object/Tasks/s3compatibleapi.htm)

OCI를 사용할 때 `S3_BUCKET=s3://<bucket>/<prefix>`와 함께
`S3_ENDPOINT_URL=https://<namespace>.compat.objectstorage.<region>.oci.customer-oci.com`을
설정하고, AWS CLI에는 OCI customer secret key를 별도 프로필로 넣는다.

```cron
0 3 * * * /opt/mazimi/scripts/backup.sh >> /var/log/mazimi-backup.log 2>&1
```

월 1회는 운영 DB가 아닌 임시 DB에 실제 복구한다. 단순히 백업 파일 존재만
확인한 것은 복구 훈련이 아니다.

```bash
make restore BACKUP=backups/wello_YYYYMMDD_HHMMSS.sql.gz
```

## 6. DNS 컷오버와 롤백

컷오버 하루 전 TTL을 300초로 낮춘다. Oracle 환경 검증이 끝난 뒤 A 레코드를
바꾸고 다음을 30분 이상 관찰한다.

- 5xx, 로그인 실패, DB pool 오류
- worker 운영 이벤트 신규 발생
- 응답 시간과 메모리
- 관리자 출처/품질 큐 수치

롤백 조건은 API health 연속 실패, 로그인 불가, 쓰기 실패, 예상하지 못한 대량
비공개다. 이 경우 DNS를 이전 주소로 되돌리고 Oracle의 API/worker를 중지해
두 환경이 동시에 쓰지 않게 한다. 같은 외부 Supabase DB를 유지하면 데이터
역복사 없이 컴퓨팅만 롤백할 수 있다.

## 7. 2단계: DB까지 Oracle로 이동

아래는 복구 훈련 성공 후에만 진행한다.

1. 기존 환경에서 `scripts/backup.sh`로 checksum 포함 최종 이전 후보를 만든다.
2. 쓰기 가능한 API와 worker를 중지한다.
3. 한 번 더 최종 백업을 만들고 파일과 `.sha256`을 Oracle VM으로 전송한다.
4. Oracle `.env.production`을 `DATABASE_MODE=local`로 바꾸고 `deploy.sh`로 빈
   PostgreSQL과 현재 이미지를 준비한다.
5. `scripts/restore.sh <backup>`으로 복원한다. 스크립트는 대상의 안전 백업,
   checksum 확인, 현재 SQLx 스키마 적용, `public` 애플리케이션 데이터의
   단일 트랜잭션 교체를 수행한다.
6. 프로그램/브랜드/탐색대상/사용자 수와 `_sqlx_migrations`를 원본과 비교한다.
7. 최소 24시간 안정화 전까지 원본 Supabase 프로젝트와 최종 백업을 삭제하지
   않는다.

DB를 VM에 둘 때는 `pgdata` 볼륨을 별도 Block Volume에 두고, 5432를 외부에
공개하지 않는다. RPO는 마지막 외부 백업 시각, RTO는 새 VM 준비 + restore
실측 시간으로 정의하고 복구 훈련 결과를 기록한다.
