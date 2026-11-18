# 임시 도메인 연결: OCI + 카카오

2026-09-23. 가비아에 로그인하지 못하는 동안의 연결입니다. 서버와 Supabase는 배포했습니다. 계정 소유자가 OCI·카카오 설정을 완료했습니다. 두 도메인 HTTPS, 앱 홈·탐색·로그인 화면, 카카오 계정 입력 화면 진입까지 실제 운영 주소에서 확인했습니다. 개인 계정 인증 후 앱으로 돌아오는 단계는 아직 실계정 검증 전입니다.

## OCI

1. Compute → Instances → 공개 IP `$MAZIMI_HOST` 인스턴스 선택.
2. Networking → Primary VNIC → Subnet → Security Lists → 적용된 보안 목록 선택.
3. Ingress Rules에 다음 두 **stateful** 규칙 추가. 기존 규칙은 삭제하지 않습니다.

| 소스 종류 | 소스 CIDR | 프로토콜 | 소스 포트 | 대상 포트 |
|---|---|---|---|---|
| CIDR | `0.0.0.0/0` | TCP | All | `80` |
| CIDR | `0.0.0.0/0` | TCP | All | `443` |

인스턴스 내부 firewalld의 HTTP/HTTPS 허용은 적용했습니다. DB·API 내부 포트는 외부 공개하지 않습니다. 적용 후 Caddy가 인증서를 자동 발급·갱신합니다.

- 앱: $MAZIMI_APP_URL
- 랜딩·관리자: $MAZIMI_API_URL
- 준비 상태: $MAZIMI_API_URL/api/ready

[OCI 보안 목록 공식 문서](https://docs.oracle.com/en-us/iaas/Content/Network/Concepts/securitylists.htm)

## 카카오

마지미 앱 → 앱 → 플랫폼 키 → **현재 서버에서 사용하는 REST API 키** → 리다이렉트 URI에 아래 주소를 추가합니다. 기존 주소는 유지합니다.

```text
$MAZIMI_API_URL/api/v1/auth/kakao/callback
```

앱과 웹(관리자 포함) 모두 이 API 콜백으로 로그인합니다. 카카오 로그인 사용 설정 ON, 프로필 닉네임·프로필 사진 동의 항목도 확인합니다. 허용 IP 제한을 사용 중이면 서버 IP `$MAZIMI_HOST`가 포함돼야 합니다. 기존 클라이언트 시크릿을 임의 변경하지 않습니다.

[카카오 설정 공식 문서](https://developers.kakao.com/docs/ko/kakaologin/prerequisite)

## 확인

- 두 주소에서 HTTPS 오류 없이 페이지가 열려야 합니다.
- 앱 → 로그인 → 카카오 로그인 → 앱 복귀를 실제 계정으로 한 번 완료합니다.
- `KOE006`: 리다이렉트 URI 불일치, `KOE004`: 로그인 기능 비활성화 여부를 확인합니다.
- 키·시크릿·로그인 코드·토큰을 채팅이나 문서에 붙이지 않습니다.

가비아 접속 복구 후 전환은 [가비아 설정 문서](mazimi-gabia-domain-setup.md)를 따릅니다. 전환 시 카카오 URI, CORS, 앱 API 주소도 함께 바꾸고 재빌드해야 합니다.
