import { ScrollView, View } from "react-native";
import Text from "@/components/Text";
import { ScreenHeader, ui } from "@/components/FigmaUI";
export default function HelpScreen() { return <View style={ui.screen}><ScreenHeader title="도움말" back /><ScrollView contentContainerStyle={[ui.content, { paddingTop: 24 }]}>{[
 ["추천된 혜택은 모두 받을 수 있나요?", "추천은 입력한 조건을 바탕으로 한 안내예요. 가입 상태, 증빙 서류, 예산 소진 여부와 제공 기관 심사에 따라 결과가 달라져요."],
 ["자동으로 신청이 완료되나요?", "마지미가 신청 정보와 서류 준비를 도와드려요. 본인인증, 약관 동의와 최종 제출은 공식 신청처에서 직접 진행해 주세요. 준비 완료와 접수 완료는 별도로 관리해요."],
 ["기업혜택 정보가 달라요", "혜택은 기업 사정에 따라 바뀔 수 있어요. 상세 화면의 ‘공식 공고 보기’에서 최신 조건을 확인해 주세요."],
 ["문의 접수 안내", "현재 별도의 문의 접수 창구는 운영하지 않아요. 문의 기능이 준비되면 안내할게요."],
].map(([title, body]) => <View key={title} style={{ marginBottom: 28 }}><Text style={ui.cardTitle}>{title}</Text><Text style={[ui.cardSummary, { fontSize: 13, lineHeight: 20 }]} selectable>{body}</Text></View>)}</ScrollView></View>; }
