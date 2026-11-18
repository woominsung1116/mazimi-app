import { BulletRow, ScreenHeader } from "@/components/FigmaUI";
import React from "react";
import {
  View,
  ScrollView,
  TouchableOpacity,
  StyleSheet,
} from "react-native";
import Text from "@/components/Text";
import { useRouter } from "expo-router";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import {
  colors,
  typography,
  spacing,
  borderRadius,
  layout,
  shadows,
} from "@/constants/theme";

export default function PrivacyPolicyScreen() {
  const router = useRouter();
  const insets = useSafeAreaInsets();

  return (
    <View style={styles.container}>
      <ScreenHeader title="개인정보처리방침" back />

      <ScrollView
        style={styles.scroll}
        contentContainerStyle={[
          styles.content,
          { paddingBottom: Math.max(insets.bottom, spacing[8]) + spacing[4] },
        ]}
        showsVerticalScrollIndicator={false}
      >
        <Text style={styles.effectiveDate}>개정일: 2026년 10월 9일 · 시행일: 2026년 10월 9일</Text>

        <Text style={styles.intro}>
          마지미(이하 "회사")는 이용자의 개인정보를 소중히 여기며, 「개인정보
          보호법」, 「정보통신망 이용촉진 및 정보보호 등에 관한 법률」 등 관련
          법령을 준수합니다. 본 방침은 회사가 운영하는 마지미 앱(이하 "서비스")
          이용과정에서 어떤 정보를 수집하고, 어떻게 사용하며, 어떻게 보호하는지
          안내합니다.
        </Text>

        <Section title="제1조 수집하는 개인정보 항목">
          <Para>
            회사는 서비스 제공을 위해 다음의 개인정보를 수집합니다.
          </Para>
          <BulletList
            items={[
              "추천을 위해 직접 입력하는 항목: 닉네임, 출생연도, 거주 지역, 학교와 학적 상태, 취업 상태, 소득 구간. 학교·학적·소득 등 선택 항목은 입력하지 않아도 둘러볼 수 있습니다.",
              "신청 도우미 이용 시: 선택한 접수 방식, 준비·확인 상태, 직접 입력한 접수번호(선택), 신청 당시 공고 내용과 기록 시각. 접수번호 입력 없이도 이용할 수 있습니다.",
              "혜택 질문 기능 이용 시: 이용자가 입력한 질문 내용. 질문은 답변을 만드는 데에만 쓰고 대화 기록으로 저장하지 않습니다.",
              "AI 비서 이용 시(제공하는 경우): 비서에게 맡긴 요청(가린 뒤 최대 300자), 비서가 만든 안내와 처리 결과, 변경 승인·취소 기록. 자세한 내용은 제6조의3을 참고하세요.",
              "소셜 로그인 시 자동 수집: 선택한 카카오 또는 Google 계정 식별자(고유 ID), 프로필 이름, 프로필 이미지(제공되는 경우). Google 이메일·메일·드라이브 접근 권한은 요청하지 않습니다.",
              "서비스 이용 중 자동 생성: 서비스 이용 기록, 접속 로그, 기기 식별자(푸시 알림 토큰)",
            ]}
          />
          <Para style={styles.noteText}>
            서류 보관함에 등록하는 파일(증명서 등)은 이용자 기기 내에서 단말기
            고유 키로 AES-256 암호화되어 저장되며, 회사 서버로 전송·저장되지
            않습니다. 자세한 내용은 제6조를 참고하세요.
          </Para>
        </Section>

        <Section title="제2조 개인정보 수집 및 이용 목적">
          <BulletList
            items={[
              "맞춤형 정책·장학금·청년 혜택 추천 및 적합도 산정",
              "서류 체크리스트 생성 및 신청 지원 안내",
              "혜택 질문에 대한 저장된 공고 정보 기반 안내",
              "AI 비서의 요청 처리, 승인한 변경의 실행·기록, 이용자가 켠 능동 알림 발송",
              "마감 임박·새 정책 등록 등 맞춤 알림 발송",
              "회원 식별 및 본인 확인",
              "서비스 이용 통계 분석 및 품질 개선",
              "관계 법령 준수 및 분쟁 처리",
            ]}
          />
        </Section>

        <Section title="제3조 개인정보 보유 및 이용 기간">
          <Para>
            회사는 원칙적으로 개인정보 수집 및 이용 목적이 달성된 후 해당
            정보를 지체 없이 파기합니다.
          </Para>
          <BulletList
            items={[
              "회원 탈퇴 시 운영 데이터베이스의 계정·프로필·신청 기록을 삭제합니다.",
              "신청 준비 체크 기록, 이용자가 직접 입력한 접수번호와 공고 사본은 신청 이력을 위해 회원 탈퇴까지 보관합니다. 마지미는 신청서를 기관에 전달하지 않으므로 기관에 전달 대기 중인 답변을 보관하지 않습니다.",
              "AI 비서 대화 기록은 마지막 활동 후 30일이 지나면 자동 삭제하며, 이용자가 기록 화면에서 언제든 바로 삭제할 수 있습니다. 내용 없이 변경 종류·결과·시각만 남기는 실행 기록은 1년 보관하고, 회원 탈퇴 시 계정과의 연결을 끊습니다.",
              "복구용 백업은 일반 서비스 조회에 사용하지 않으며, 기본 7일 보관 후 정기 정리합니다. 삭제 직전 생성된 백업에는 정리 시점까지 기록이 남을 수 있습니다.",
              "법령에 따라 별도 보관이 필요한 경우에는 해당 근거와 보관 대상을 안내합니다.",
            ]}
          />
        </Section>

        <Section title="제4조 개인정보의 제3자 제공">
          <Para>
            회사는 이용자의 개인정보를 원칙적으로 외부에 제공하지 않습니다.
            다만, 다음의 경우에는 예외로 합니다.
          </Para>
          <BulletList
            items={[
              "소셜 로그인: 선택한 카카오 또는 Google의 인증 방식을 사용하며, 해당 제공자의 개인정보처리방침이 적용됩니다. 회사는 계정 식별과 프로필 표시에 필요한 정보만 수신합니다.",
              "이용자의 사전 동의가 있는 경우",
              "법령에 의거하거나 수사 기관의 적법한 요청이 있는 경우",
            ]}
          />
          <Para style={styles.noteText}>
            혜택 질문의 AI 처리는 기본적으로 꺼져 있습니다. 운영 설정으로 켜진 경우에도
            이용자가 질문 화면에서 동의한 질문만 회사가 직접 운영하는 비공개 AI 모델로
            처리하며, 외부 AI 서비스에는 전송하지 않습니다. 처리 목적은 질문에 맞는 공고 원문
            문장을 바탕으로 답변 요약을 작성하는 것이며, 전송되는 것은 질문 내용과 서버가 찾은
            공고 원문 문장뿐입니다. 질문의 이메일·링크·긴 숫자는 전송 전에 가리도록 처리합니다(모든
            형태의 개인정보를 탐지하지는 못하므로 질문에 개인정보를 적지 말아 주세요). 프로필·보관
            서류·계정 정보는 전송하지 않습니다. 동의하지 않아도 공고 원문 인용과 저장된 공고 정보
            기반 안내, 질문으로 혜택 찾기(규칙 기반)를 이용할 수 있습니다.
          </Para>
        </Section>

        <Section title="제5조 개인정보 처리 위탁">
          <Para>
            회사는 서비스 운영을 위해 아래와 같이 개인정보 처리를 위탁할 수
            있으며, 위탁 계약 시 개인정보가 안전하게 관리될 수 있도록 필요한
            사항을 규정합니다.
          </Para>
          <BulletList
            items={[
              "API·수집 작업 서버 운영: Oracle Cloud Infrastructure",
              "계정·프로필·신청 기록 데이터베이스 운영: Supabase",
              "푸시 알림: Google Firebase Cloud Messaging(FCM)을 연동하는 경우 알림 토큰을 사용합니다. 현재 운영 서버의 푸시 발송 연동은 설정 전입니다.",
            ]}
          />
        </Section>

        <Section title="제6조 서류 보관함(Document Vault) 관련 개인정보 처리">
          <Para style={styles.subHeading}>수집 서류 항목</Para>
          <BulletList
            items={[
              "주민등록등본·초본, 가족관계증명서, 소득증명원, 재학·휴학·졸업증명서, 건강보험료 납부확인서·자격득실확인서, 통장사본, 장애인증명서 등 이용자가 직접 등록하는 서류 파일 및 그 메타데이터(서류 종류·발급일·만료일)",
            ]}
          />
          <Para style={[styles.subHeading, { marginTop: spacing[3] }]}>처리 목적</Para>
          <BulletList
            items={[
              "신청 서류 체크리스트 관리 및 신청 지원",
              "체크 목록 항목과 보관 서류 이름의 일치 여부 표시",
              "서류 발급일 기준 갱신 시점 안내",
            ]}
          />
          <Para style={[styles.subHeading, { marginTop: spacing[3] }]}>보유 기간 및 파기</Para>
          <BulletList
            items={[
              "이용자가 보관함에서 직접 삭제하는 즉시 기기에서 파기됩니다.",
              "회원 탈퇴 시 이 기기에 있는 해당 계정의 보관 서류를 삭제합니다. 다른 기기나 앱 외부로 내보낸 사본은 이용자가 해당 위치에서 삭제해야 합니다.",
              "서류의 만료일이 지나거나 발급 후 90일이 경과하면 '갱신 필요' 상태로 표시되며, 파기 여부는 이용자가 직접 결정합니다(자동 삭제되지 않음).",
            ]}
          />
          <Para style={[styles.subHeading, { marginTop: spacing[3] }]}>제3자 제공</Para>
          <BulletList
            items={[
              "마지미는 신청서를 대리 작성하거나 기관에 제출하지 않습니다. 신청 정보 입력과 제출은 이용자가 직접 진행하며, 앱은 공고별 체크 목록, 공식 안내 링크와 제6조의2의 본인 작성 보조 기능을 제공합니다.",
              "앱이 서류를 자동으로 외부 제공하지 않습니다. 이용자가 공유·내보내기를 선택하면 선택한 앱이나 제공처로 파일을 전달할 수 있습니다.",
            ]}
          />
          <Para style={[styles.subHeading, { marginTop: spacing[3] }]}>민감정보(건강 관련 서류) 별도 동의</Para>
          <BulletList
            items={[
              "건강보험료 납부확인서·자격득실확인서 등 건강 상태를 유추할 수 있는 서류를 업로드하려는 경우, 다른 서류와 분리된 별도의 동의 절차를 거칩니다(개인정보 보호법 제23조).",
              "해당 동의를 거부하더라도 등본·소득증명 등 다른 서류의 보관·이용 기능은 동일하게 제공됩니다. 필수 서비스 이용에 민감정보 동의를 조건으로 걸지 않습니다.",
            ]}
          />
          <Para style={[styles.subHeading, { marginTop: spacing[3] }]}>보관 위치 및 안전조치</Para>
          <BulletList
            items={[
              "서버 전송 없음: 서류 파일은 회사 서버로 전송·저장되지 않고 이용자 기기 내에만 보관됩니다.",
              "파일은 계정별 기기 보관 키를 이용해 AES-256으로 암호화하고 무결성을 확인합니다. 앱이 만든 임시 평문 사본은 처리 후 삭제하며, 이용자가 원래 보유한 파일은 변경하지 않습니다.",
              "서류 원본은 회사 서버에 업로드하지 않습니다. 브라우저에서는 파일 보관 기능을 제공하지 않습니다.",
              "기기 분실·초기화 시 복구 불가: 이용자가 직접 관리에 책임을 집니다.",
              "앱 삭제 시: 기기에 저장된 모든 로컬 서류 데이터가 함께 삭제됩니다.",
            ]}
          />
        </Section>

        <Section title="제6조의2 서류 작성 도우미(기기 내 양식 작성) 관련 처리">
          <Para>
            서류 작성 도우미는 기관이 공개한 빈 신청 양식에 이용자가 직접 입력한
            내용을 이용자 기기 안에서 옮겨 적어 주는 무료 본인 작성 보조 기능입니다.
            회사는 작성한 신청서를 대리 작성하거나 기관에 제출하지 않습니다.
          </Para>
          <Para style={styles.subHeading}>처리하는 항목</Para>
          <BulletList
            items={[
              "양식에 이용자가 직접 입력하는 기재 사항: 성명, 생년월일, 연락처, 주소, 이메일, 학교·학과·학년·학번, 은행명·예금주, 주민등록번호, 계좌번호, 손으로 그린 서명, 이름 도장 이미지 등 양식마다 요구하는 항목",
              "제안값: 프로필의 학교명·학년, 오늘 날짜, 이용자가 켠 경우 기기에 기억한 성명·연락처·주소·이메일. 제안값은 화면에 표시만 되며 이용자가 확인하거나 고쳐야 양식에 반영됩니다.",
            ]}
          />
          <Para style={[styles.subHeading, { marginTop: spacing[3] }]}>처리 위치</Para>
          <BulletList
            items={[
              "입력 내용과 작성한 신청서 파일은 회사 서버로 전송·저장되지 않고 이용자 기기 안에서만 처리됩니다.",
              "빈 양식은 기관이 공개한 주소에서 기기가 직접 내려받으며, 회사가 확인한 원본과 크기·해시가 다르면 사용하지 않습니다. 회사 서버에는 양식의 입력 칸 위치 정보만 있고 이용자 입력 값은 없습니다.",
              "문서 미리보기는 기기 안의 웹뷰에서 그리며, 이때 공개 PDF 표시 프로그램(pdf.js)을 공개 배포망(jsDelivr)에서 내려받습니다. 양식 파일과 입력 내용은 배포망으로 전송되지 않습니다.",
            ]}
          />
          <Para style={[styles.subHeading, { marginTop: spacing[3] }]}>주민등록번호·계좌번호</Para>
          <BulletList
            items={[
              "작성하는 동안에만 입력받아 앱 메모리에서 사용하며, 기기나 서버에 저장하지 않습니다. 회사는 이 정보를 수집하지 않습니다.",
              "작성본을 내보낸 직후, 또는 작성 화면을 나가면 앱 메모리에서 지웁니다. 기억하기 대상이 아니며 제안값으로도 표시하지 않습니다.",
            ]}
          />
          <Para style={[styles.subHeading, { marginTop: spacing[3] }]}>보관과 파기</Para>
          <BulletList
            items={[
              "기억하기(선택): 이용자가 켠 경우에만 성명·연락처·주소·이메일을 기기 보안 저장소(iOS Keychain / Android Keystore)에 암호화해 저장합니다. 작성 화면의 '기억한 정보 지우기' 또는 회원 탈퇴 시 삭제됩니다.",
              "작성 기록: 작성 일시, 사업명, 양식 이름과 양식 버전(해시)만 기기에 남기며 입력 내용은 기록하지 않습니다. 회원 탈퇴 시 삭제됩니다.",
              "작성본 파일: 이용자가 선택한 저장 위치나 앱으로만 전달되며, 앱이 만든 임시 사본은 전달 직후 삭제합니다. 내보낸 파일은 이용자가 직접 관리합니다.",
              "금융상품 신청서는 이 기능의 대상이 아닙니다.",
            ]}
          />
        </Section>

        <Section title="제6조의3 AI 혜택 비서 관련 처리">
          <Para>
            AI 비서는 기본적으로 꺼져 있으며, 운영 설정으로 제공을 시작한 경우에만
            앱에 표시됩니다. 비서는 저장된 공고 정보와 회사의 규칙으로 혜택 찾기, 조건 확인,
            준비 목록 정리를 하고, 관심 저장·알림·준비 체크·신청 상태 기록·프로필 항목 수정처럼
            정보를 바꾸는 일과 서식 작성 도우미를 초안과 함께 여는 일은 변경 전후를 보여 주는 확인
            카드에서 이용자가 승인한 경우에만 실행합니다. 기관 사이트 로그인, 신청서 제출, 결제는
            하지 않습니다.
          </Para>
          <Para style={styles.subHeading}>처리하는 항목과 보관</Para>
          <BulletList
            items={[
              "요청 문장: 이메일·링크·긴 숫자를 가린 뒤 최대 300자를 대화 기록으로 저장합니다. 주민등록번호·연락처·계좌번호는 적지 말아 주세요(모든 형태를 탐지하지는 못합니다).",
              "처리 결과: 비서가 보여 준 혜택 목록, 조건 확인 결과, 준비 항목, 공고 인용과 승인 카드 내용·처리 결과를 대화 기록에 함께 저장합니다.",
              "프로필 수정 카드: 바꿀 수 있는 항목은 거주 지역, 학적 상태, 취업 상태, 학년, 소득분위(학자금 지원구간), 가구원 수뿐입니다. 바꿀 항목과 승인 전·후 값은 승인 카드 기록에 저장되고 대화 기록과 같은 기간 보관·삭제됩니다.",
              "서류 준비 상태: 준비 서류와 서류 보관함의 비교는 이용자 기기 안에서만 합니다. 보관 서류의 이름·종류·발급일은 회사 서버나 AI 모델로 보내지 않습니다.",
              "서식 초안: 서식 작성 도우미의 초안은 기기 안에서 프로필과 이 기기에 기억한 정보로 만들고, 주민등록번호·계좌번호는 채우지 않습니다. 입력값은 서버나 AI 모델로 보내지 않습니다.",
              "보관 기간: 마지막 활동 후 30일이 지나면 자동 삭제합니다. 기록 화면에서 대화별로 즉시 삭제할 수 있고, 회원 탈퇴 시 모두 삭제됩니다. 삭제한 기록은 모델 평가나 학습에 쓰지 않습니다.",
              "실행 기록: 승인·취소·만료된 변경의 종류와 결과, 시각만 1년 보관하며 요청 문장이나 혜택 내용은 남기지 않습니다. 회원 탈퇴 시 계정과의 연결을 끊습니다.",
            ]}
          />
          <Para style={[styles.subHeading, { marginTop: spacing[3] }]}>AI 모델 처리</Para>
          <BulletList
            items={[
              "자주 쓰는 요청은 AI 모델 없이 규칙으로 바로 처리합니다.",
              "규칙으로 처리하지 못한 요청은 AI 처리가 켜진 경우에만, 가린 요청 문장과 '보고 있던 혜택이 있는지, 고른 혜택이 몇 개인지'만 회사가 직접 운영하는 비공개 AI 모델에 보내 처리 순서를 정합니다. 외부 AI 서비스에는 전송하지 않습니다.",
              "프로필, 혜택 원문, 서류, 계정 정보, 이전 대화는 모델에 보내지 않습니다. 조건 충족 여부와 금액·기간은 모델이 아니라 저장된 공고 정보와 규칙으로 정합니다.",
            ]}
          />
          <Para style={[styles.subHeading, { marginTop: spacing[3] }]}>알림</Para>
          <BulletList
            items={[
              "처리에 시간이 걸린 요청은 끝났을 때 푸시로 알려 드리며, 알림에는 요청 내용을 넣지 않습니다.",
              "능동 알림(주간 할 일 브리핑, 저장한 혜택의 공고 변경, 멈춘 준비)은 모두 꺼져 있고 이용자가 켠 항목만, 알림 설정에서 푸시를 허용한 경우 오전 9시~오후 9시에 하루 최대 1건 보냅니다.",
              "공고 변경 알림을 켜면 저장한 혜택의 마감·조건·서류·금액 비교값을 보관하고, 끄면 삭제합니다.",
            ]}
          />
        </Section>

        <Section title="제7조 이용자 및 법정대리인의 권리">
          <Para>
            이용자(만 14세 미만의 경우 법정대리인)는 언제든지 다음 권리를
            행사할 수 있습니다.
          </Para>
          <BulletList
            items={[
              "개인정보 열람 요청",
              "프로필 정보 확인·정정: 앱의 내 정보 편집에서 직접 변경",
              "개인정보 삭제 요청 (회원 탈퇴 시 자동 처리)",
              "개인정보 처리 정지 요청",
              "회원 탈퇴: 내 정보 화면에서 직접 처리",
            ]}
          />
          <Para>
            현재 별도의 문의 접수 창구는 운영하지 않습니다. 추가 권리 행사
            접수 방법과 담당자 연락처는 준비 후 안내하겠습니다.
          </Para>
        </Section>

        <Section title="제8조 개인정보의 파기 절차 및 방법">
          <BulletList
            items={[
              "전자 파일 형태: 복원 불가능한 기술적 방법으로 영구 삭제",
              "종이 출력물: 분쇄 또는 소각 (해당하는 경우)",
              "보유 기간이 만료되거나 처리 목적이 달성된 경우 지체 없이 파기",
            ]}
          />
        </Section>

        <Section title="제9조 개인정보 안전성 확보 조치">
          <BulletList
            items={[
              "전송 구간 암호화: HTTPS/TLS 1.2 이상 적용",
              "접근 통제: 개인정보 취급 직원 최소화 및 권한 관리",
              "기기 내 저장 데이터: iOS Keychain / Android Keystore를 통한 암호화",
              "서류 보관함 파일: 서버로 전송되지 않고 이용자 기기 내에서만 AES-256으로 암호화되어 저장됩니다 — 회사 서버는 애초에 평문 서류를 전송받거나 열람할 수 없는 구조입니다.",
              "정기적 보안 점검 및 취약점 분석",
            ]}
          />
        </Section>

        <Section title="제10조 쿠키 및 유사 기술 사용">
          <Para>
            앱 설정과 동의 상태는 기기 또는 브라우저의 로컬 저장소에 보관합니다.
            앱의 로그인 토큰은 기기 보안 저장소, 웹 앱의 로그인 토큰은 브라우저
            세션 저장소에 보관합니다. 관리자 웹의 로그인에는 세션 쿠키를 사용합니다.
            로그아웃이나 브라우저 저장소 설정에서 관련 데이터를 제거할 수 있습니다.
          </Para>
        </Section>

        <Section title="제11조 개인정보처리방침의 변경">
          <Para>
            본 방침은 법령 변경, 정책 변경 등에 따라 내용이 추가·삭제·수정될
            수 있습니다.
          </Para>
          <BulletList
            items={[
              "변경 시 앱 내 공지사항 또는 알림을 통해 사전 고지합니다 (최소 7일 전, 중요 변경 시 30일 전).",
              "동의가 필요한 변경은 기존 서비스 이용만으로 동의한 것으로 처리하지 않습니다.",
              "단, 새로운 개인정보 처리 목적이 추가되는 등 중요한 변경의 경우 앱 실행 시 재동의 절차를 통해 별도로 동의를 받습니다.",
            ]}
          />
          <Para style={styles.noteText}>
            개정 이력 — AI 비서 제공 시작 시 적용: AI 혜택 비서의 요청·처리 결과 저장, 30일 자동 삭제,
            비공개 모델 처리 범위, 승인형 변경과 능동 알림, 신청 준비 연결(기기 안 서류 대조·서식 초안)과
            프로필 항목 수정 카드(제6조의3)를 추가했습니다.
            2026.10.12: 서류 작성 도우미(기기 내 양식 작성)의 처리 항목과
            보관·파기 방식을 추가했습니다. 2026.10.09: 제공하지 않는 기관 연결 신청 관련 수집·제공 안내를
            삭제하고, 보관 서류 처리 목적과 혜택 질문 처리 방식을 실제 기능에 맞췄습니다.
            2026.09.23: 실제 입력 항목, Oracle·Supabase 운영 구성,
            백업 보관, 서류 공유와 계정별 보관 방식, 문의 창구 운영 상태를 반영했습니다.
          </Para>
        </Section>

        <Section title="제12조 개인정보 보호책임자">
          <Para>
            개인정보 담당자와 권리 행사 접수 창구는 준비 중입니다.
            담당자 연락처가 확정되면 이 화면에 안내하겠습니다.
          </Para>
          <View style={styles.contactCard}>
            <ContactRow label="문의 접수" value="현재 운영하지 않음" isLast />
          </View>
          <Para>
            개인정보 침해로 인한 신고나 상담은 아래 기관에도 문의하실 수 있습니다.
          </Para>
          <BulletList
            items={[
              "개인정보 침해신고센터: privacy.kisa.or.kr / 118",
              "개인정보 분쟁조정위원회: www.kopico.go.kr / 1833-6972",
              "대검찰청 사이버수사과: www.spo.go.kr / 1301",
              "경찰청 사이버수사국: ecrm.cyber.go.kr / 182",
            ]}
          />
        </Section>

        <Text style={styles.footer}>
          본 개인정보처리방침은 2026년 10월 9일부터 적용됩니다.
        </Text>
      </ScrollView>
    </View>
  );
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <View style={styles.section}>
      <Text style={styles.sectionTitle}>{title}</Text>
      {children}
    </View>
  );
}

function Para({
  children,
  style,
}: {
  children: React.ReactNode;
  style?: object;
}) {
  return <Text style={[styles.para, style]}>{children}</Text>;
}

function BulletList({ items }: { items: string[] }) {
  return (
    <View style={styles.bulletList}>
      {items.map((item, i) => (
        <BulletRow key={i} style={styles.bulletText}>{item}</BulletRow>
      ))}
    </View>
  );
}

function ContactRow({
  label,
  value,
  isLast = false,
}: {
  label: string;
  value: string;
  isLast?: boolean;
}) {
  return (
    <View style={[styles.contactRow, !isLast && styles.contactRowDivider]}>
      <Text style={styles.contactLabel}>{label}</Text>
      <Text style={styles.contactValue}>{value}</Text>
    </View>
  );
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

const styles = StyleSheet.create({
  container: {
    flex: 1,
    backgroundColor: colors.surfaceContainerLow,
  },

  header: {
    flexDirection: "row",
    alignItems: "center",
    backgroundColor: colors.surfaceContainerLowest,
    paddingHorizontal: layout.pagePadding,
    paddingBottom: spacing[3],
    ...shadows.header,
  },
  backButton: {
    width: layout.touchTargetMin,
    height: layout.touchTargetMin,
    alignItems: "center",
    justifyContent: "center",
    marginLeft: -spacing[2],
  },
  headerTitle: {
    flex: 1,
    textAlign: "center",
    fontSize: typography.fontSize.md, lineHeight: 24,
    fontWeight: typography.fontWeight.bold,
    color: colors.onSurface,
  },
  headerRight: {
    width: layout.touchTargetMin,
  },

  scroll: {
    flex: 1,
  },
  content: {
    paddingHorizontal: layout.pagePadding,
    paddingTop: spacing[6],
    gap: spacing[6],
  },

  effectiveDate: {
    fontSize: typography.fontSize.xs, lineHeight: 16,
    fontWeight: typography.fontWeight.regular,
    color: colors.textMuted,
    marginBottom: -spacing[2],
  },

  intro: {
    fontSize: typography.fontSize.sm,
    fontWeight: typography.fontWeight.regular,
    color: colors.textSecondary,
    lineHeight: 20,
  },

  section: {
    gap: spacing[3],
  },
  sectionTitle: {
    fontSize: typography.fontSize.base,
    fontWeight: typography.fontWeight.bold,
    color: colors.onSurface,
    lineHeight: 20,
  },

  para: {
    fontSize: typography.fontSize.sm,
    fontWeight: typography.fontWeight.regular,
    color: colors.textSecondary,
    lineHeight: 20,
  },
  subHeading: {
    fontSize: typography.fontSize.sm,
    fontWeight: typography.fontWeight.semibold,
    color: colors.onSurface,
    lineHeight: 20,
  },
  noteText: {
    fontSize: typography.fontSize.xs,
    color: colors.textMuted,
    lineHeight: 16,
    backgroundColor: colors.surfaceContainerHigh,
    padding: spacing[3],
    borderRadius: borderRadius.md,
  },

  bulletList: {
    gap: spacing[2],
  },
  bulletText: {
    flex: 1,
    fontSize: typography.fontSize.sm,
    fontWeight: typography.fontWeight.regular,
    color: colors.textSecondary,
    lineHeight: 20,
  },

  contactCard: {
    backgroundColor: colors.surfaceContainerLowest,
    borderRadius: borderRadius.lg,
    paddingHorizontal: layout.cardPadding,
    ...shadows.card,
  },
  contactRow: {
    flexDirection: "row",
    justifyContent: "space-between",
    alignItems: "center",
    paddingVertical: spacing[3],
    minHeight: layout.touchTargetMin,
  },
  contactRowDivider: {
    borderBottomWidth: 0.5,
    borderBottomColor: colors.outlineVariant,
  },
  contactLabel: {
    fontSize: typography.fontSize.sm, lineHeight: 20,
    fontWeight: typography.fontWeight.medium,
    color: colors.textSecondary,
  },
  contactValue: {
    fontSize: typography.fontSize.sm, lineHeight: 20,
    fontWeight: typography.fontWeight.semibold,
    color: colors.onSurface,
  },

  footer: {
    fontSize: typography.fontSize.xs,
    color: colors.textMuted,
    textAlign: "center",
    lineHeight: 16,
    paddingTop: spacing[2],
  },
});
