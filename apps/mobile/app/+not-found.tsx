import { Stack, useRouter } from "expo-router";
import { View } from "react-native";
import { EmptyState, ui } from "@/components/FigmaUI";

// Expo Router의 기본 Unmatched Route 화면(영문, 사이트맵 링크)을 대신한다.
export default function NotFoundScreen() {
  const router = useRouter();
  return <View style={[ui.screen, { justifyContent: "center" }]}>
    <Stack.Screen options={{ headerShown: false, title: "페이지를 찾을 수 없어요" }} />
    <EmptyState title="페이지를 찾을 수 없어요" description="주소가 바뀌었거나 없어진 페이지예요." action="홈으로 가기" onAction={() => router.replace("/(tabs)")} />
  </View>;
}
