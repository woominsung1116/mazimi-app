import React from "react";
import { ScrollView, View } from "react-native";
import { ScreenHeader, ui } from "@/components/FigmaUI";
import AssistantBriefingCard from "@/components/AssistantBriefingCard";
import NonMemberState from "@/components/NonMemberState";
import { useAuthStore } from "@/store/auth";

/** 저장한 혜택, 다음 할 일: opened from the 홈 banner for every member, whatever the 비서 flag says. */
export default function BriefingScreen() {
  const user = useAuthStore(s => s.user);
  return <View style={ui.screen}>
    <ScreenHeader title="다음 할 일" back />
    {user ? <ScrollView contentContainerStyle={ui.content}><AssistantBriefingCard /></ScrollView>
      : <NonMemberState title="로그인하고 다음 할 일을 확인해 보세요" description="저장한 혜택과 준비 기록을 바탕으로 할 일을 정리해요." />}
  </View>;
}
