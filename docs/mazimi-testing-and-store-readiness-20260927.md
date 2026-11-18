# 마지미 테스트와 앱스토어 준비 상태

2026-09-27 확인. **Windows 웹 테스트는 가능하며, App Store 정식 출시 준비가 모두 끝난 상태는 아닙니다.**

## 지금 테스트

- Windows Chrome/Edge: $MAZIMI_APP_URL
- 같은 주소를 iPhone Safari/Android Chrome에서도 열 수 있습니다. 웹 테스트는 실제 네이티브 앱 테스트를 대체하지 않습니다.
- 로그인 → 지역·학교 선택 → 혜택 탐색 → 신청 도우미(접수 방식·체크 목록·공식 안내 링크) → 신청 이력 순으로 확인합니다.
- 실제 신청처 열기는 외부 기관으로 이동합니다. 테스트할 때 접수 완료를 허위로 기록하거나 실제 기관에 시험 신청서를 보내지 않습니다.
- 앱은 신청서를 대신 작성하거나 기관에 제출하지 않습니다. 2026-10-02에 기관 연결 제출(Google Forms 커넥터)과 "신청 자료 자동 준비" 화면을 제거했습니다. 이전 검증 기록(`audits/submission-*.json`, `audits/*application-kit*.json`)은 이력으로만 남습니다.

## 현재 확인된 네이티브 설정

- Expo SDK 54 / React Native 앱 구조, iOS bundle identifier `kr.mazimi.app`, Android package `kr.mazimi.app`.
- `apps/mobile/eas.json`에 기기 테스트·preview·production 프로필이 있습니다.
- 기기/preview/production 빌드는 현재 오라클 API 주소를 명시하고 `.env.local`의 localhost가 포함되지 않도록 설정했습니다.
- 이번 작업에서 서명된 iOS IPA를 만들거나 TestFlight/App Store에 업로드하지 않았습니다. Apple Developer 가입 상태, 인증서·프로비저닝·App Store Connect 앱 등록 상태도 확인하지 못했습니다.
- iOS 시뮬레이터를 Windows에서 직접 실행하는 방식은 아닙니다. EAS 클라우드 빌드/제출은 Windows에서도 가능하며, 실제 설치 테스트에는 iPhone이 필요합니다. [Expo EAS 제출](https://docs.expo.dev/submit/ios/)

## 팀원 Android APK 빌드 (2026-10-02 갱신)

Android preview 빌드를 배포하고 있습니다. 최신 버전은 팀원 문서에서 확인합니다. APK 다운로드 링크, 설치·테스트 순서와 팀원 재빌드 명령은 **[팀원 설치·테스트·개발 안내](mazimi-team-guide.md)**에서 관리합니다.

팀원 문서에 연결한 빌드 검증 기록에 빌드 ID, 소스 커밋과 파일 SHA-256을 남깁니다. 파일 무결성과 운영 API 주소 포함을 확인했으며 실기기 설치·로그인·신청 흐름 검증은 아직 남아 있습니다.

## 출시 전 남은 실제 항목

1. Apple Developer 계정·서명·App Store Connect 설정, 배포 빌드, iPhone 실기기 로그인·파일 선택/공유·알림·계정 탈퇴 테스트.
2. 현재 카카오 로그인 외에 Apple 심사 4.8의 요건을 만족하는 동등한 로그인 수단 검토·구현. 마지미의 현재 일반 소비자 앱 구조에는 예외가 확인되지 않았으며 Sign in with Apple을 추가하는 방향이 적절합니다. Google 로그인 추가만으로 이 요건을 충족했다고 단정할 수 없습니다. [Apple 4.8](https://developer.apple.com/app-store/review/guidelines/#login-services)
3. 실제 연락 가능한 앱 내 지원 경로와 Support URL. 사용자의 기존 요청에 따라 문의 창구는 아직 운영하지 않습니다. 정식 출시 전에는 이 부분이 바뀌어야 합니다. [Apple 1.5](https://developer.apple.com/app-store/review/guidelines/#developer-information)
4. 심사자가 이용할 계정/접근 방법, 스크린샷·설명·개인정보 수집 항목 신고·공개 개인정보처리방침 URL. 출시 설명과 스크린샷에 "자동신청·자동 입력·대리 제출" 표현을 쓰지 않습니다. 앱이 하는 일은 공식 신청처 안내, 본인 입력용 체크 목록, 사용자가 직접 기록하는 신청 이력, 기기 내 서류 보관입니다.

TestFlight는 정식 App Store 공개 전에 iPhone에 설치해 확인하는 경로입니다. 유료 Apple Developer 계정이 필요하며 외부 테스터 배포에는 Beta App Review가 적용될 수 있습니다. [Expo TestFlight](https://docs.expo.dev/submit/testflight/), [Apple 앱 검토](https://developer.apple.com/app-store/review/).
