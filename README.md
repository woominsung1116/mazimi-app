# 마지미 (Mazimi)

![마지미 — 당신에게 꼭 맞는 혜택이 마중 나오는 순간](docs/images/mazimi-keyvisual.jpg)

청년을 위한 **정책·장학금·기업·대학·금융 혜택 추천 플랫폼**

프로필(지역, 나이, 학교·학적, 소득구간, 취업상태 등)에 따라 수집한 혜택을 추천하고, 서류 준비·공식 신청 안내·신청 이력을 제공합니다.

## 팀원이 먼저 확인할 내용

- **[APK 다운로드·설치·테스트·이어서 개발하기](docs/mazimi-team-guide.md)** — 팀원에게 이 문서를 전달하면 됩니다.
- 현재 웹 앱(`$MAZIMI_APP_URL`, [운영 변수](docs/ops-env.md)) — Windows Chrome/Edge에서도 테스트할 수 있습니다.
- [테스트·앱스토어 준비 상태](docs/mazimi-testing-and-store-readiness-20260927.md)
- [AI 혜택 비서 기술 스펙](docs/mazimi-ai-assistant-technical-spec.md) — 안내·상황별 후보·근거 비교의 구현 계약, 로컬 100문항 평가와 미완료 출시 게이트
- [오라클 운영](docs/oracle-runbook.md), [가비아 도메인 연결](docs/mazimi-gabia-domain-setup.md), [임시 도메인 설정](docs/mazimi-temporary-domain-console-setup.md)
- Figma 원본은 `apps/mobile/마지미 (공유용).fig`입니다. 앱 화면은 이 디자인을 기준으로 구현했습니다.

2026-09-27 기준으로 웹 앱·API·수집 워커를 오라클에 배포했습니다. 마지미는 신청서를 대신 작성하거나 기관에 제출하지 않습니다. 신청 도우미는 공식 신청처 링크, 접수 방식·서류 체크 목록, 사용자가 직접 기록하는 신청 이력, 기기 내 서류 보관함으로 구성됩니다. 현재 iOS 실기기 검증과 App Store 제출은 완료되지 않았습니다. Google 로그인은 설정 전까지 비활성화됩니다.

저장소에는 코드·디자인 원본·마이그레이션·운영 문서를 포함합니다. `.env`·운영 비밀키·DB 백업·로컬 실행 기록·빌드 산출물은 포함하지 않습니다. 팀원은 환경변수 예시를 복사하고 필요한 실제 키를 별도로 설정해야 합니다.

## 주요 기능

- **카카오 1-Tap 로그인** — OAuth 기반 간편 인증 (모바일/웹)
- **맞춤 추천** — 프로필 기반 정책·장학금 자동 매칭
- **신청 도우미** — 공고별 접수 방식, 저장 가능한 체크 목록, 공식 신청처·공고 원문 링크. 본인 입력 보조이며 대신 작성·제출하지 않음
- **기업·대학 혜택** — 구조화된 제공자 피드·공식 소스 수집, 학교 선택 시 대학 탭 표시
- **서류 보관함** — 기기 내 AES-256 암호화 저장, 만료 알림, 체크 목록과 이름 대조 힌트
- **놓친 돈 계산기** — 미신청 혜택 예상 금액 시각화
- **조건 시뮬레이터** — 지역/소득 변경 시 혜택 변화 비교
- **신청 상태 관리** — 7단계 트래킹 (관심 → 계획 → 신청중 → 완료 → 대기 → 수혜 → 포기)
- **마감 알림** — D-7, D-3, D-1 푸시 알림
- **비로그인 접근** — 홈/탐색/상세/계산기는 로그인 없이 사용 가능
- **관리자 대시보드** — 프로그램 CRUD, 통계, 수동 동기화

## 아키텍처

```
┌──────────┐     ┌──────────┐     ┌──────────┐
│  Caddy   │────▶│ Next.js  │     │  Worker  │
│ (Reverse │     │  Web App │     │ (Cron /  │
│  Proxy)  │──┐  └──────────┘     │  Ingest) │
└──────────┘  │                   └────┬─────┘
              │  ┌──────────┐         │
              └─▶│ Rust API │         │
                 │ (Axum)   │◀────────┤
┌──────────┐     └────┬─────┘         │
│  Expo    │          │               │
│  Mobile  │──────────┘               │
│  App     │  ┌───────▼───────┐       │
└──────────┘  │  PostgreSQL   │◀──────┘
              │  (Supabase)   │
              └───────────────┘
```

## 기술 스택

| 영역 | 기술 |
|------|------|
| 모바일 앱 | React Native (Expo 54), TypeScript, Zustand, Expo Router |
| 웹 프론트엔드 | Next.js 16, TypeScript, Tailwind CSS v4, TanStack Query, Zustand |
| 백엔드 API | Rust, Axum 0.8, SQLx |
| 워커 | Rust, tokio-cron-scheduler, reqwest |
| DB | PostgreSQL 16 (Supabase), 버전 관리 SQLx 마이그레이션 |
| 인증 | 카카오 OAuth (모바일: expo-auth-session, 웹: NextAuth.js) |
| 보안 | JWT + RLS + Rate Limiting + AES-256 서류 암호화 |
| 디자인 시스템 | 공유 Figma 원본, Pretendard, 앱 공통 UI 컴포넌트 |
| 배포 | Docker Compose, Caddy, GitHub Actions → ghcr.io |

## 프로젝트 구조

```
.
├── apps/
│   ├── mobile/             # React Native (Expo) 모바일 앱
│   │   ├── app/            # Expo Router 화면
│   │   │   ├── (tabs)/     # 홈, 탐색, 관리, 프로필
│   │   │   ├── onboarding/ # 4단계 온보딩 (닉네임→지역→나이/학적→취업상태)
│   │   │   └── programs/   # 프로그램 상세
│   │   ├── components/     # 공통 컴포넌트
│   │   ├── constants/      # 테마, 디자인 토큰
│   │   ├── lib/            # API, 암호화, 알림 유틸
│   │   └── store/          # Zustand 상태 관리
│   └── web/                # Next.js 웹 프론트엔드
│       └── src/
│           └── app/        # 페이지 (홈, 로그인, 관리자 등)
├── crates/
│   ├── api/                # Axum HTTP API 서버
│   ├── wello-core/         # 공유 도메인 모델, 설정
│   └── worker/             # 크론 작업, 공공 API 수집
├── packages/
│   ├── config/             # 공유 설정
│   └── ui/                 # 공유 UI 패키지
├── infra/
│   ├── docker/             # Dockerfile (api, web, worker)
│   ├── caddy/              # Caddyfile (prod, dev)
│   └── migrations/         # SQLx 스키마·기준 데이터 마이그레이션
├── scripts/
│   ├── scraper/            # Python 크롤러 (Scrapling)
│   ├── deploy.sh           # 프로덕션 배포 스크립트
│   └── backup.sh           # DB 백업 스크립트
├── docs/                   # 문서 (API, 배포, 기능, 개인정보처리방침 등)
├── compose.yml             # 프로덕션 Docker Compose
├── compose.dev.yml         # 개발 Docker Compose
└── Makefile                # 개발 명령어 단축
```

## 데이터 소스

| 소스 | API | 건수 |
|------|-----|------|
| 행안부 공공서비스 | api.odcloud.kr | ~10,900건 |
| 한국장학재단 | api.odcloud.kr | ~1,600건 |
| 온통청년 | youthcenter.go.kr | - |
| 금감원 금융상품 | finlife.fss.or.kr | - |
| 드림스폰 장학금 | dreamspon.com | - |

매일 소스를 수집·정규화하고 만료된 혜택을 공개 목록에서 제외합니다. 신청한 공고의 사본과 이력은 유지합니다.

## 시작하기

### 사전 요구사항

- Docker & Docker Compose
- Rust toolchain (로컬 개발 시)
- Node.js 20.19.4 이상 (웹·모바일 개발), pnpm (웹 프론트엔드)
- Expo CLI (모바일 앱)

### 개발 환경 실행

```bash
# 환경변수 설정
cp .env.example .env.dev

# Docker Compose로 백엔드 스택 실행
make dev

# 개별 접속
# - 웹: http://localhost:3000
# - API: http://localhost:8080
# - DB: localhost:5432
```

### 모바일 앱 실행

```bash
cd apps/mobile
npm ci
cp .env.example .env.local
npx expo start --web
```

Windows PowerShell에서는 복사 명령을 `Copy-Item .env.example .env.local`로 실행할 수 있습니다. 네이티브 앱 실행 시 `npx expo start`를 사용합니다. iPhone 배포 빌드와 TestFlight는 별도의 Apple 계정·서명 설정이 필요합니다.

`releases/mobile`은 생성된 정적 웹 파일이므로 Git에서 제외합니다. 배포할 API 주소를 설정한 뒤 `apps/mobile`에서 `npx expo export --platform web --output-dir ../../releases/mobile`로 생성합니다. 운영 내보내기는 로컬 환경 파일의 영향을 막도록 `EXPO_NO_DOTENV=1`과 `EXPO_PUBLIC_API_URL`을 명시하세요.

### 주요 명령어

```bash
make dev          # 개발 환경 시작 (docker compose up --build)
make dev-down     # 개발 환경 종료
make dev-logs     # 로그 확인
make db-shell     # PostgreSQL 접속
make prod         # 프로덕션 환경 시작
make clean        # 볼륨 포함 전체 정리
```

### 로컬 Rust 개발 (Docker 없이)

```bash
# API 서버
cargo run --bin api

# 워커
cargo run --bin worker

# 테스트
cargo test --workspace
```

`api` 크레이트는 Unix 전용 파일 API(`read_exact_at`)를 써서 Windows에서는 빌드되지 않습니다. Windows에서는 WSL이나 Linux에서 빌드하고, 빌드 산출물은 `CARGO_TARGET_DIR`로 용량이 넉넉한 드라이브에 둡니다(디버그와 릴리스 합쳐 10GB 안팎). `core.autocrlf=true`로 받은 작업 사본은 줄바꿈이 CRLF로 바뀌어 sha256을 고정한 파일(`crates/api/assets/intent-classifier/classifier.json`, `crates/api/tests/fixtures/agent_goals_holdout*.json`)이 맞지 않습니다. 분류기가 적재되지 않고 고정 테스트 세 개가 실패하므로, 이 파일들은 `git show HEAD:<경로> > <경로>`로 LF 원본을 받아 쓰거나 `core.autocrlf=false`로 받은 사본에서 테스트합니다.

## CI/CD

- **PR → main**: `cargo check` + `cargo clippy` + `cargo test` + `pnpm build` + ESLint 자동 실행
- **Push → main**: 검증 및 Docker 빌드. 비공개 저장소의 레지스트리 이미지 발행은 기본 비활성화합니다. 패키지 공개 범위를 확인하고 저장소 변수 `ENABLE_IMAGE_PUBLISH=true`를 명시한 경우에만 발행합니다.

## 환경변수

`.env.example` 참조. 주요 변수:

| 변수 | 설명 |
|------|------|
| `DATABASE_URL` | PostgreSQL 연결 문자열 |
| `JWT_SECRET` | JWT 서명 키 |
| `KAKAO_CLIENT_ID/SECRET` | 카카오 OAuth 인증 |
| `SUPABASE_URL` | Supabase 프로젝트 URL |
| `KAKAO_BIZAPI_KEY` | 카카오 비즈 API (알림톡) |
| `FCM_PROJECT_ID` | Firebase Cloud Messaging |
| `EXPO_PUBLIC_API_URL` | 모바일 앱 API 엔드포인트 |

## 라이선스

Private — All rights reserved.
