# 마지미 Google 로그인 설정

2026-09-23. 앱의 Google 로그인 코드와 서버 검증을 구현했습니다. 아직 Google OAuth 클라이언트가 없으므로 운영 버튼은 ‘Google 로그인 준비 중’으로 표시됩니다. 설정 여부 API는 사용 가능 여부만 반환하며 비밀키를 노출하지 않습니다.

## 1. Google에서 클라이언트 만들기

1. [Google Cloud Console](https://console.cloud.google.com/)에서 팀 소유 프로젝트를 선택하거나 생성합니다.
2. Google Auth Platform의 Branding에서 앱 이름 `마지미`, 실제 관리 가능한 사용자 지원·개발자 연락처를 입력합니다. 이 연락처는 Google 설정에 필요하며 마지미에 문의 기능을 추가하는 작업과는 별개입니다.
3. Audience는 일반 사용자를 위한 External로 설정합니다. 먼저 Testing 상태에서 팀원의 Google 계정을 테스트 사용자로 등록합니다.
4. Data Access는 `openid`, `https://www.googleapis.com/auth/userinfo.profile`만 사용합니다. 이메일·Gmail·Drive 접근은 필요 없습니다.
5. Clients → Create client → **Web application**을 선택합니다. 모바일 웹과 앱 모두 서버 콜백에서 인증 코드를 교환하므로 웹 클라이언트를 사용합니다.
6. 승인된 리디렉션 URI에 다음 주소를 정확히 등록합니다. 끝에 `/`를 추가하지 않습니다.

```text
$MAZIMI_API_URL/api/v1/auth/google/callback
```

자바스크립트에서 Google 토큰을 직접 교환하지 않으므로 승인된 JavaScript 원본은 현재 흐름의 필수 설정이 아닙니다. 이 콜백을 앱 주소인 `$MAZIMI_APP_URL/login`으로 바꾸면 안 됩니다. 로그인 후 앱 복귀 주소는 마지미 서버가 따로 관리합니다.

임시 `sslip.io` 도메인의 등록·브랜딩 검증을 Google이 제한하면 그 검증을 우회하지 않습니다. 소유권 확인 가능한 `mazimi.kr`의 DNS 연결을 먼저 완료하고 아래 정식 콜백을 등록해야 합니다. 임시 도메인으로 정식 공개 검증이 통과된 상태는 아닙니다.

## 2. Oracle 서버에 설정

클라이언트 ID와 비밀번호를 `/opt/mazimi/.env.production`의 다음 항목에 저장합니다. 비밀번호는 Git, 화면 캡처, 프런트엔드 환경변수, 공개 문서에 넣지 않습니다.

```dotenv
GOOGLE_CLIENT_ID=발급된_클라이언트_ID
GOOGLE_CLIENT_SECRET=발급된_클라이언트_비밀번호
```

기존 항목·키는 유지하고 파일 권한 `600`을 유지합니다. 해당 파일은 Compose의 API `env_file`로 이미 연결되어 있습니다.

```sh
cd /opt/mazimi
docker compose --env-file .env.production up -d --no-build --no-deps --force-recreate api
curl -fsS "$MAZIMI_API_URL"/api/v1/auth/providers
```

응답의 `google: true`는 설정 값이 존재한다는 뜻입니다. 실제 Google 설정 유효성까지 인증한 값은 아닙니다. 앱의 로그인 화면을 다시 열어 테스트 계정으로 로그인 → Google 계정 선택 → 동의 → 마지미 복귀를 확인합니다. 신규 계정은 온보딩, 기존 계정은 홈으로 이동합니다. 취소 후 재시도도 확인합니다.

카카오 계정과 Google 계정은 서로 다른 마지미 계정입니다. 이름이나 이메일이 같다는 이유로 기록을 자동 합치지 않습니다. 기존 사용자는 이전에 선택한 로그인 방식을 계속 사용해야 합니다. 계정 연결은 별도 본인 확인 기능을 만든 후 제공해야 합니다.

## 3. 정식 도메인 전환

[가비아 연결 안내](mazimi-gabia-domain-setup.md)를 완료한 뒤 Google에 다음 콜백을 추가합니다.

```text
https://mazimi.kr/api/v1/auth/google/callback
```

Google Branding의 홈페이지·개인정보처리방침·약관 주소와 도메인 소유권을 확인하고, 서버의 `DOMAIN=mazimi.kr`, `APP_DOMAIN=app.mazimi.kr`, 앱의 API 주소를 함께 전환합니다. HTTPS와 실계정 복귀를 확인한 다음 오래된 임시 콜백을 제거합니다. Google의 공개·검증 요구 사항을 충족하기 전에는 테스트 사용자 범위에서 검증합니다.

## 구현과 검증 범위

- 요청별 state, Google PKCE S256 및 OIDC nonce 사용.
- Google 공개키로 RS256 서명, issuer, audience, expiry, nonce, subject 검증.
- 마지미 액세스·리프레시 토큰은 리디렉션 URL에 넣지 않음.
- 앱 복귀 코드는 제공자·앱 검증자에 묶이고 2분 후 만료, 한 번만 교환 가능.
- Google 제공자 토큰은 계정 조회에 필요한 기간만 메모리에서 사용하며 DB에 저장하지 않음.
- 기본 프로필 권한만 요청. 외부 혜택 사이트 로그인·신청 권한을 함께 받는 기능이 아님.
- 테스트용 RSA 키는 `crates/api/tests/fixtures/oauth-test-only.private`이며 운영 키가 아님.
- 실제 Google 계정 복귀 검증은 클라이언트 발급 후 진행해야 함.

공식 자료: [웹 서버 OAuth](https://developers.google.com/identity/protocols/oauth2/web-server), [OpenID Connect 검증](https://developers.google.com/identity/openid-connect/openid-connect), [Google 로그인 디자인](https://developers.google.com/identity/branding-guidelines). 버튼은 Google이 제공한 PNG 원본을 사용합니다.
