# 마지미 팀원 설치·테스트·개발 안내

2026-10-02 갱신. Android preview APK 빌드는 성공했으며, 실기기 설치·로그인·신청 흐름 검증은 아직 남아 있습니다.

## 바로 사용하는 링크

| 용도 | 링크 |
| --- | --- |
| Android APK 설치 파일 | [마지미 preview APK 다운로드](https://expo.dev/artifacts/eas/cQnXWf4vbd6CgfXeN8a8yE4KFWrSOsXiBw59n4efXhs.apk) |
| 위 APK의 빌드 기록 | [Expo 빌드 상세](https://expo.dev/accounts/mazimi/projects/majimi/builds/4014c6f4-5582-4605-8b90-b68315003817) |
| 다음 빌드 확인 | [Expo 전체 빌드 목록](https://expo.dev/accounts/mazimi/projects/majimi/builds) |
| Windows·iPhone·Android 웹 테스트 | 현재 웹 앱(`$MAZIMI_APP_URL`) |
| 코드 | [GitHub mazimi-app](https://github.com/woominsung1116/mazimi-app) |
| 파일 검증 결과 | [APK 검증 기록·SHA-256](audits/android-preview-icon-build-20261002.json) |

위 APK는 **2026-10-02에 빌드한 고정 파일**입니다. 앱 버전 `1.0.0`, Android versionCode `4`, 소스 커밋 [`9a21480`](https://github.com/woominsung1116/mazimi/commit/9a214805bea00477b5aa79e8991a25aee005a032), 파일 크기는 약 89 MB입니다. 새 코드가 GitHub에 올라와도 이 링크의 APK가 자동으로 바뀌지는 않습니다. 다운로드 링크가 열리지 않으면 빌드 상세에서 다운로드 가능 여부를 확인합니다.

이번 APK에는 새로 제공된 집 모양 앱 아이콘과 원본 Figma 시작 화면·온보딩 디자인과 지역/학교 필터, 로그인 세션 복구 수정이 포함됩니다. 원본 초록 배경·흰 로고가 실제 APK의 5개 화면 밀도 이미지와 일치함을 검증했습니다. 이전 APK를 설치했다면 **새 APK를 다시 설치**합니다. 기존 서명 키와 패키지를 유지하므로 앱을 먼저 삭제할 필요가 없습니다. 삭제하면 기기에 보관된 서류와 암호화 키가 사라질 수 있습니다.

전체 결과는 [종합 점검 보고서](audits/overall-product-20261002.md)에 정리했습니다. 독립 점검 기록: [지역·추천·API](audits/api-region-recommend-20261002.md), [모바일 상태](audits/mobile-state-20261002.md), [수집·신청·알림](audits/service-safety-20261002.md), [통합 검증](audits/service-regression-20261002.json).

## 설치해서 테스트하기

1. Android 기기에서 APK 다운로드 링크를 열고 파일을 내려받습니다.
2. 다운로드한 APK를 열어 설치합니다. Android에서 해당 브라우저나 파일 앱의 설치 허용을 요청하면 그 앱에 한해 허용합니다.
3. 설치된 **마지미**를 실행합니다. Expo Go나 별도의 개발 서버는 필요하지 않습니다. [Expo APK 설치 안내](https://docs.expo.dev/build-reference/apk/#physical-device)
4. 아래 순서로 기능을 확인하고 결과를 기록합니다.

| 확인할 기능 | 확인 내용 |
| --- | --- |
| 첫 실행 | 원본 초록 시작 화면·흰 로고, 약관/로그인 진입, 글꼴·이미지 표시와 재실행 |
| 로그인 | 카카오 인증 후 앱 복귀, 로그인 유지와 로그아웃 |
| 온보딩 | 지역·학교·학적 선택 및 저장, 학교 미선택 시 대학 혜택 노출 여부 |
| 혜택 탐색 | 부산 설정 후 홈 추천·탐색의 내 지역에 용인 전용 공고가 나오지 않는지, 지역 변경·학교 미선택·상세 화면 |
| 신청 준비 | 신청 방법 표시, 공식 신청처 열기, 준비 자료·체크 목록·신청 이력 |
| 서류 기능 | 개인정보가 없는 샘플 파일로 선택·보관·공유 동작 |

이 APK는 운영 API `$MAZIMI_API_URL`에 연결합니다. 본인 테스트 계정으로 확인하고, 실제 기관에 시험 신청서를 제출하거나 접수하지 않은 신청을 접수 완료로 기록하지 않습니다. 앱은 신청서를 대신 작성하거나 제출하지 않습니다. 신청 도우미는 공식 신청처 링크와 본인 입력용 체크 목록만 제공합니다.

아이콘 설정은 iOS에도 적용돼 있지만 서명된 iOS 빌드·TestFlight 배포·실기기 검증은 아직 하지 않았습니다. APK는 Android용입니다. iPhone과 Windows 브라우저에서는 위 웹 앱 링크로 테스트합니다. iOS 설치 배포와 App Store 준비 항목은 [출시 준비 상태](mazimi-testing-and-store-readiness-20260927.md)를 확인합니다.

## 코드를 받아 이어서 개발하기

준비물은 Git, Node.js **20.19.4 이상**, 저장소 접근 권한입니다. Expo 재빌드는 `mazimi` 조직의 Developer 이상 권한이 있는 본인 계정을 사용합니다. [Expo 역할 안내](https://docs.expo.dev/accounts/account-types/#manage-access)

처음 받는 경우:

```sh
git clone https://github.com/woominsung1116/mazimi-app.git
cd mazimi
```

기존 작업 폴더가 있다면 저장소 루트에서 `git pull --ff-only`로 최신 코드를 받습니다. 이어서 모바일 앱 의존성을 설치합니다.

```sh
cd apps/mobile
npm ci
```

로컬 웹 개발은 `.env.example`을 `.env.local`로 복사한 다음 `npm run web`으로 실행합니다. Windows PowerShell에서는 `Copy-Item .env.example .env.local`, macOS/Linux에서는 `cp .env.example .env.local`을 사용합니다. 이미 자신의 환경 설정이 있다면 파일 내용을 확인해 필요한 값을 맞춥니다.

수정한 코드와 필요한 설정을 GitHub에 커밋·push하면 다른 팀원도 그 지점부터 이어서 작업할 수 있습니다. 모바일 앱을 재빌드할 때 운영 비밀키나 서명 키를 전달할 필요는 없습니다. Android 서명 키는 Expo에서 관리합니다.

## 새 Android APK 빌드하기

`apps/mobile` 디렉터리에서 실행합니다.

```sh
npx eas-cli@latest login
npx eas-cli@latest project:info
npx eas-cli@latest build --platform android --profile preview
```

프로젝트 확인 결과는 **`@mazimi/majimi`**여야 합니다. 실제 Expo 프로젝트 slug는 `majimi`이며, 표시 이름은 마지미입니다. 프로젝트 ID는 `225a8c33-91f4-4993-beb6-1114b74b58fa`, Android 패키지는 `kr.mazimi.app`입니다. 기존 EAS 관리 Android 서명 키를 사용합니다.

`preview` 프로필은 Android versionCode를 자동 증가시키며 APK 형식과 운영 API 주소를 지정합니다. 또한 로컬 `.env.local`의 API 주소를 빌드에 사용하지 않도록 설정돼 있습니다. 빌드가 성공하면 CLI 출력 또는 Expo 빌드 상세에서 APK를 받습니다. 기존 `npm run build:preview` 명령은 iOS도 함께 요청하므로 Android 테스트에는 위 명령을 사용합니다.

새 테스트 빌드를 공유할 때는 이 문서의 APK 링크·빌드 기록·버전·소스 커밋을 함께 갱신합니다. APK 원본은 Expo에서 배포하고 Git 저장소에는 안내와 검증 결과를 남깁니다.

## 테스트 결과 전달하기

[GitHub Issues](https://github.com/woominsung1116/mazimi-app/issues)에 아래 내용을 남기면 재현하기 쉽습니다.

```text
기기 / Android 버전:
APK 빌드 링크 또는 소스 커밋:
재현 순서:
기대한 결과:
실제 결과:
발생 빈도:
```

스크린샷이나 로그에는 로그인 토큰·개인 서류·타인의 개인정보가 들어가지 않도록 확인합니다. 빌드 성공은 실기기 기능 검증 완료를 뜻하지 않으므로 위 테스트 결과를 별도로 기록합니다.
