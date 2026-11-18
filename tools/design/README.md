# Figma 원본 리소스와 UI 검증

앱 런타임과 분리된 개발 도구입니다. 기준 파일은 `apps/mobile/마지미 (공유용).fig`입니다.

```sh
npm ci --prefix tools/design
npm run export --prefix tools/design
npm run check:native --prefix tools/design
```

`export.mjs`는 고정된 refig 버전으로 Figma 노드를 해석합니다. 로고·아이콘은 원본 path, transform, opacity, winding rule을 보존한 SVG와 4배 PNG로 내보냅니다. 단색 아이콘은 앱에서 tint할 수 있도록 검정으로 내보냅니다. 삽화는 원본 ZIP 안의 해당 이미지에서 내보냅니다. `assets/figma/source.json`에 파일 SHA-256, 노드 ID, 이미지 해시를 기록합니다. 앱 아이콘의 여백은 Android adaptive icon의 안전 영역을 고려합니다.

`check:native`는 **apps/mobile에 설치된 Expo SDK 54의 실제 Android 스플래시 생성 함수**로 밝은/어두운 모드의 5가지 밀도 이미지를 생성합니다. 원본 초록색 배경, 흰 로고, 파란 픽셀 부재를 확인합니다. `apps/mobile`에 먼저 `npm ci`가 필요합니다. 저장소 루트에서 `MAZIMI_APK=releases/android/파일.apk node tools/design/check-native-assets.mjs`를 실행하면 실제 APK 안의 리소스를 읽어 생성한 5개 밀도의 픽셀과 일치하는지도 검사합니다. APK는 Git에 넣지 않습니다.

브라우저 동작·화면 검증은 다음 순서로 실행합니다.

```sh
cd apps/mobile
EXPO_NO_DOTENV=1 EXPO_PUBLIC_API_URL="$MAZIMI_API_URL" npx expo export --platform web --output-dir ../../releases/mobile-figma
cd ../../tools/design
npx playwright install chromium
npm run check:ui
```

Windows에서는 셸에 맞게 위 환경변수를 설정합니다. 이미 설치한 Chromium을 사용하려면 `MAZIMI_CHROMIUM`에 실행 파일 경로를 지정합니다.

브라우저 검증은 로컬 서버와 고정 API 응답만 사용합니다. 운영 회원정보를 수정하거나 카카오 인증·실제 기관 신청을 실행하지 않습니다. 첫 실행 필수 동의, 약관 이동, 로그인 선택, 지역·나이·학교, 대학 탭 숨김, 주요 화면과 320px 레이아웃을 확인하고 `docs/design/validation/figma-20261001/`에 화면을 저장합니다. APK 실기기 검사와 구분합니다.

GitHub CI에서도 매 push/PR마다 모바일 타입·세션 검사 후 원본 스플래시 픽셀 검사, 웹 내보내기, 위 브라우저 흐름을 실행합니다. 결과와 화면은 `mobile-regression-evidence` 아티팩트로 14일 보관합니다. API는 고정 fixture로 가로채므로 운영 계정이나 기관 접수를 사용하지 않습니다.

앱 아이콘은 2026-10-02 사용자가 제공한 집 모양 원본(`apps/mobile/assets/brand/app-icon-original.png`)을 사용합니다. `node tools/design/export-app-icon.mjs`로 1024px 아이콘·Android 마스크용 여백·파비콘을 만듭니다. Figma 전체 재추출도 이 원본을 사용하므로 예전 글자 아이콘으로 되돌아가지 않습니다. 스플래시는 별도의 Figma 원본 흰 글자 자산을 유지합니다.

`check:native`는 Expo가 만드는 일반·원형·adaptive 아이콘 15개도 생성합니다. `MAZIMI_APK`를 지정하면 APK 내 아이콘 15개와 Figma 스플래시 5개를 생성 결과와 픽셀 단위로 대조합니다. 아이콘 원본·출력 해시는 `assets/brand/source.json`으로 관리합니다.

## Figma 토큰과 화면 검사

```sh
npm run tokens --prefix tools/design        # .fig → apps/mobile/constants/figma-tokens.json
npm run check:tokens --prefix tools/design  # 커밋된 토큰이 .fig와 같은지
npm run test:figma-tokens --prefix apps/mobile
npm run check:figma --prefix tools/design   # 웹 export 필요. 390px 화면과 Figma 비교 이미지
```

토큰 파일의 `texts`는 .fig에 있는 모든 글자입니다. `apps/mobile/scripts/check-copy.cjs`는 앱(`app/`·`components/`·`lib/`)과 서버(`crates/api/src`, 테스트 제외)가 쓰는 문장이 이 목록이나 `scripts/copy-allowlist.json`(보여 주는 이유별)에 있을 때만 통과시킵니다.

기준과 유지 항목은 [Figma 복원 문서](../../docs/design/figma-restore-20261026.md)에 있습니다. `npm run export`는 아이콘·삽화와 함께 앱 아이콘도 다시 만듭니다. 현재 `export-app-icon.mjs` 출력은 커밋된 앱 아이콘과 다르니, 아이콘만 다시 뽑을 때는 앱 아이콘 파일 변경을 되돌리세요.

## 카테고리 아이콘과 색

카테고리별 이미지·글리프·색은 `apps/mobile/constants/categoryStyle.ts` 한 표에 있습니다(2026-10-31 사용자 결정). 원본 출처와 해시는 `apps/mobile/assets/categories/source.json`에 있습니다.

```sh
node tools/design/export-category-heroes.mjs <png-1024 폴더>   # 상세 hero 640px
npm run category-colors --prefix tools/design                  # 이미지에서 타일·강조색 계산
npm run check:category-colors --prefix tools/design            # 표가 이미지와 같은지 (CI)
```
