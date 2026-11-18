# Mazimi 배포 가이드

전체 흐름: Oracle Cloud VM 생성 → DNS 설정 → Docker 설치 → 코드 배포 → SSL 자동 발급

---

## 1. Oracle Cloud VM 생성

운영 대상은 **Oracle Cloud Infrastructure(OCI) Compute**로 정한다. 무료 사용
가능 여부, 홈 리전 제한, 인스턴스 재고는 시점과 계정에 따라 달라질 수 있으므로
[OCI Free Tier 공식 문서](https://docs.oracle.com/iaas/Content/FreeTier/freetier.htm)와
실제 콘솔 표시를 기준으로 선택한다.

권장 운영 구성은 다음과 같다.

- Oracle VM: Caddy, Web, API, worker
- Supabase: PostgreSQL
- VM 아키텍처: Ampere A1이면 ARM64, 일반 Compute면 x86_64
- 메모리: 외부 Supabase 사용 기준 최소 2 GB, 여유 있게 4 GB 이상 권장

1. OCI 콘솔에서 Compute Instance를 생성한다.
2. Image는 Ubuntu 24.04 LTS를 선택한다.
3. SSH 공개 키를 등록하고 공인 IP를 할당한다.
4. VCN Security List 또는 Network Security Group에서 TCP 22, 80, 443과
   UDP 443 인바운드만 허용한다.
5. 생성된 공인 IP를 DNS A 레코드에 사용한다.

저장소의 Docker 이미지는 ARM64와 x86_64 모두에서 대상 서버 아키텍처에 맞춰
직접 빌드한다. 저사양 VM에서는 최초 Rust 이미지 빌드가 오래 걸릴 수 있다.

---

## 2. 도메인 연결 (DNS A 레코드)

DNS 공급자(가비아, Cloudflare 등)에서 아래 레코드를 추가한다.

| 유형 | 이름 | 값 | TTL |
|------|------|----|-----|
| A | `@` (또는 `mazimi.kr`) | VPS IP | 300 |
| A | `www` | VPS IP | 300 |

Cloudflare 사용 시 **Proxy(오렌지 구름)를 OFF** 하고 DNS-only(회색 구름)로 설정해야 Caddy가 Let's Encrypt TLS 챌린지를 처리할 수 있다.

전파 확인:
```bash
dig mazimi.kr +short
# VPS IP가 반환되면 준비 완료
```

---

## 3. SSH 접속 + 서버 초기 설정

```bash
ssh ubuntu@<ORACLE_VM_IP>

# 시스템 업데이트
apt update && apt upgrade -y

# 방화벽 설정
ufw allow 22/tcp   # SSH
ufw allow 80/tcp   # HTTP (Caddy)
ufw allow 443/tcp  # HTTPS (Caddy)
ufw allow 443/udp  # HTTP/3 (QUIC)
ufw --force enable
ufw status
```

---

## 4. Docker 설치

```bash
# 저장소의 재현 가능한 부트스트랩 사용
git clone https://github.com/woominsung1116/mazimi-app.git /tmp/mazimi-bootstrap
sudo bash /tmp/mazimi-bootstrap/scripts/oracle-bootstrap.sh
# docker 그룹 반영을 위해 SSH 재접속

# 버전 확인
docker version
docker compose version
```

---

## 5. Git clone + 환경변수 설정

```bash
# bootstrap이 준비한 프로젝트로 이동
cd /opt/mazimi

# 환경변수 파일 생성
cp .env.production.example .env.production
nano .env.production   # 또는 vim
```

`.env.production`에서 반드시 변경해야 하는 항목:

| 변수 | 설명 | 예시 |
|------|------|------|
| `DOMAIN` | 실제 도메인 | `mazimi.kr` |
| `DATABASE_URL` | PostgreSQL DSN | `postgres://wello:pw@db:5432/wello` |
| `DATABASE_MODE` | `local` 또는 `external` | `external` |
| `DB_PASSWORD` | PG 비밀번호 | openssl rand -base64 32 |
| `JWT_SECRET` | Axum JWT 키 | openssl rand -base64 48 |
| `KAKAO_CLIENT_ID` | 카카오 REST API 키 | Kakao Developers 콘솔 |
| `KAKAO_CLIENT_SECRET` | 카카오 Client Secret | Kakao Developers 콘솔 |

deploy.sh가 `CHANGE_ME` 플레이스홀더를 감지하면 자동으로 secrets를 생성한다.

---

## 6. deploy.sh 실행

```bash
cd /opt/mazimi
chmod +x scripts/deploy.sh scripts/backup.sh

./scripts/deploy.sh
```

스크립트가 자동으로 수행하는 작업:
1. Docker / openssl 설치 여부 확인
2. `.env.production` 없으면 example에서 복사
3. `CHANGE_ME` 플레이스홀더 → openssl로 자동 생성
4. `docker compose build --parallel`
5. DB 헬스체크 후 SQLx 마이그레이션 실행
6. 전체 서비스 기동 (`docker compose up -d`)
7. API DB·스키마 readiness 검증 (`/ready`)
8. 배포 완료 URL 출력

---

## 7. SSL 자동 발급 (Caddy)

Caddy는 HTTPS 첫 요청 시 Let's Encrypt에서 TLS 인증서를 자동 발급한다. 별도 설정 불필요.

- 인증서 저장 위치: `caddy_data` Docker volume
- 자동 갱신: Caddy가 만료 30일 전 자동 갱신
- www → apex 리다이렉트: Caddyfile에 설정됨

**확인 방법:**
```bash
# Caddy 로그에서 인증서 발급 확인
docker compose -f /opt/mazimi/compose.yml logs caddy | grep -i "certificate"

# HTTPS 응답 확인
curl -I https://mazimi.kr
```

---

## 8. 모니터링 확인

### 서비스 상태
```bash
make status
# 또는
docker compose -f /opt/mazimi/compose.yml ps
```

### 로그 실시간 확인
```bash
make logs
# 특정 서비스만
docker compose -f /opt/mazimi/compose.yml logs -f api
docker compose -f /opt/mazimi/compose.yml logs -f web
```

### 헬스 엔드포인트
```bash
curl https://mazimi.kr/api/health
# {"status":"ok"}
```

### 리소스 사용량
```bash
docker stats
```

### 기업 혜택 재검증 워커

`worker`는 AI가 아니라 DB 작업과 정기 작업을 수행하는 Rust 백그라운드
프로세스다. 외부에 포트를 열지 않고 Docker Compose가 `unless-stopped` 정책으로
계속 실행한다.

```bash
# 실행 및 헬스 상태
docker compose ps worker

# 재검증/변경 감지 로그
docker compose logs -f --tail=200 worker

# 스케줄을 기다리지 않고 1회 실행
docker compose run --rm --no-deps worker revalidate-benefits
```

운영 동작은 다음과 같다.

1. 승인된 공식 출처를 6시간마다 확인 대상으로 조회한다.
2. 본문 해시가 달라지면 연결된 혜택을 즉시 비공개하고 관리자 검토 큐로 보낸다.
3. 리다이렉트 이탈, 404/410, 접근 차단은 즉시 보류한다.
4. 일시적 네트워크 실패는 재시도하고 연속 3회 실패하면 보류한다.
5. 검증 기한과 72시간 유예를 모두 넘긴 출처도 자동으로 보류한다.
6. 관리자가 변경 없음/내용 갱신/종료 중 하나로 처리하면 감사 이력을 남기고,
   모든 연결 출처가 정상인 혜택만 다시 공개한다.

`BENEFIT_CRAWLER_ENABLED=false`가 기본값이다. 이 상태에서도 오래된 혜택의
자동 보류는 계속 작동한다. 직접 페이지 접근은 해당 도메인의 이용약관,
robots 정책, 허용 경로와 내부 승인이 모두 확인된 뒤에만 도메인별로 활성화한다.

---

## 9. 자동 백업 설정 (crontab)

```bash
# 매일 새벽 3시 자동 백업 + 7일 보관
crontab -e

# 아래 줄 추가:
0 3 * * * /opt/mazimi/scripts/backup.sh >> /var/log/mazimi-backup.log 2>&1
```

수동 백업:
```bash
make backup
# 또는
./scripts/backup.sh
```

백업·복구는 애플리케이션용 로컬 DB 이미지와 분리된 PostgreSQL 17
클라이언트 컨테이너를 사용한다. 따라서 PostgreSQL 17 기반 Supabase와 기존
PostgreSQL 16 로컬 볼륨을 모두 다룰 수 있다. Supabase의 메이저 버전이
바뀌면 `POSTGRES_CLIENT_IMAGE`도 같은 버전 이상으로 올린다.

백업에는 Supabase가 관리하는 내부 스키마·전용 확장을 넣지 않고 `public`
애플리케이션 데이터만 저장한다. 복구 시 현재 코드의 SQLx 마이그레이션으로
스키마를 먼저 만든 후, 같은 트랜잭션 안에서 기존 앱 데이터를 비우고 백업
데이터를 넣는다. 이 방식으로 Supabase 백업을 일반 PostgreSQL 기반 Oracle
환경에도 복구할 수 있으며 `_sqlx_migrations`는 현재 배포 버전을 유지한다.

백업 파일 위치: `/opt/mazimi/backups/wello_YYYYMMDD_HHMMSS.sql.gz`

---

## 10. Supabase 마이그레이션 (선택)

로컬 PostgreSQL 대신 Supabase managed DB를 사용하려면:

1. [Supabase](https://supabase.com) 프로젝트 생성 (Seoul 리전 선택)

2. **Connect → Session pooler**에서 connection string 복사

3. `.env.production` 수정:
   ```
   DATABASE_MODE=external
   DATABASE_URL=postgres://postgres.[ref]:[password]@aws-0-ap-northeast-2.pooler.supabase.com:5432/postgres?sslmode=require
   SUPABASE_URL=https://[ref].supabase.co
   SUPABASE_SERVICE_ROLE_KEY=[service_role_key]
   ```

   API와 worker는 장시간 실행되는 SQLx 서비스이므로 prepared statement를
   지원하지 않는 transaction pooler(`6543`)를 사용하지 않는다.

4. `./scripts/deploy.sh` 실행. 스크립트는 로컬 PostgreSQL을 시작하지 않고
   `api-migrate`로 `infra/migrations`를 순서대로 적용한 뒤 API/worker를 기동한다.

5. Supabase SQL Editor/Table Editor에서 운영 스키마를 직접 수정하지 않는다.
   모든 스키마·기준 데이터 변경은 저장소의 마이그레이션 파일로 관리한다.

---

## 업데이트 배포 (코드 변경 후 재배포)

```bash
cd /opt/mazimi
git pull origin main
make deploy
```

`make deploy`는 `./scripts/deploy.sh`를 호출하여 이미지 재빌드 + 롤링 재시작을 수행한다.

---

## 롤백

```bash
# 이전 Git 커밋으로 되돌리기
git log --oneline -5
git checkout <commit-hash>
make deploy
```

---

## 문제 해결

| 증상 | 확인 명령 |
|------|----------|
| 서비스 시작 안 됨 | `make logs` |
| DB 연결 실패 | `docker compose exec db pg_isready -U wello` |
| SSL 발급 실패 | `docker compose logs caddy` — DNS 전파 확인 |
| API 500 에러 | `docker compose logs api` |
| 메모리 부족 | `docker stats` → 플랜 업그레이드 고려 |
