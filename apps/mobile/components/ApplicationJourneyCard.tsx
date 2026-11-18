import React, { useState } from "react";
import { Pressable, TextInput, View } from "react-native";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import Text from "./Text";
import { FigmaIcon, ui } from "./FigmaUI";
import { api, type ApplicationJourney } from "@/lib/api";
import { apiErrorText } from "@/lib/errorCopy";
import { useAuthStore } from "@/store/auth";
import { invalidateAfterWrite } from "@/lib/writeEffects";

export default function ApplicationJourneyCard({ journey }: { journey: ApplicationJourney }) {
  const client = useQueryClient();
  const user = useAuthStore(s => s.user);
  const [confirmed, setConfirmed] = useState(false);
  const [receipt, setReceipt] = useState("");
  const save = useMutation({ mutationFn: (outcome: 'unknown' | 'reported_applied') => api.reportApplicationJourney(journey.program_id, journey.id, outcome, outcome === 'reported_applied' && confirmed, outcome === 'reported_applied' ? receipt.trim() || undefined : undefined),
    onSuccess: async result => {
      client.setQueryData(['application-journey', user?.id, journey.program_id], { journey: result });
      await invalidateAfterWrite(client, 'record_application_status', { userId: user?.id, programId: journey.program_id });
    },
    // The request may have reached the server: re-read every row a journey report changes.
    onError: () => { void invalidateAfterWrite(client, 'record_application_status', { userId: user?.id, programId: journey.program_id }); }
  });
  return <View style={[ui.benefitCard, { marginTop: 20, gap: 12 }]}>
    <Text style={ui.cardTitle}>{journey.outcome === 'reported_applied' ? '접수 완료 · 내가 확인한 기록' : journey.outcome === 'unknown' ? '접수 여부 확인 필요' : '신청을 이어서 진행하세요'}</Text>
    <Text style={ui.muted}>{journey.snapshot.channel.label} · {new Date(journey.handoff_at).toLocaleDateString('ko-KR')} 준비 저장</Text>
    {journey.outcome === 'reported_applied' ? <>
      {!!journey.receipt_reference && <Text style={ui.cardSummary}>내가 입력한 접수번호: {journey.receipt_reference}</Text>}
    </> : <>
      <Text style={ui.cardSummary}>{journey.outcome === 'unknown' ? '이미 제출했다면 다시 신청하기 전에 기관의 접수 내역을 먼저 확인해 주세요.' : '안내 페이지 열기나 이메일 발송만으로 접수가 완료되지는 않아요. 기관의 접수 내역을 확인한 뒤 기록해 주세요.'}</Text>
      <TextInput accessibilityLabel="내가 확인한 접수번호" placeholder="접수번호 (선택)" maxLength={120} value={receipt} onChangeText={setReceipt} autoCapitalize="none" autoCorrect={false} style={{ padding: 14, backgroundColor: '#F7F9FA', borderRadius: 12, fontFamily: 'Pretendard', fontSize: 13, lineHeight: 20 }} />
      <Pressable accessibilityRole="checkbox" accessibilityLabel="기관의 접수 완료 안내를 직접 확인했어요" aria-checked={confirmed} accessibilityState={{ checked: confirmed }} onPress={() => setConfirmed(!confirmed)} style={{ flexDirection: 'row', alignItems: 'center', gap: 10, minHeight: 44 }}>
        <FigmaIcon name="checkCircle" size={24} color={confirmed ? '#34BB84' : '#E1E1E1'} /><Text style={{ fontSize: 13, lineHeight: 20, flex: 1 }}>기관의 접수 완료 안내를 직접 확인했어요</Text>
      </Pressable>
      <Pressable accessibilityRole="button" disabled={!confirmed || save.isPending} onPress={() => save.mutate('reported_applied')} style={[ui.primaryButton, (!confirmed || save.isPending) && { opacity: 0.45 }]}><Text style={ui.primaryButtonText}>확인한 접수 완료 기록</Text></Pressable>
      <Pressable accessibilityRole="button" disabled={save.isPending || journey.outcome === 'unknown'} onPress={() => save.mutate('unknown')} style={ui.outlineButton}><Text style={ui.outlineButtonText}>{journey.outcome === 'unknown' ? '접수 확인이 필요한 상태로 저장했어요' : '접수됐는지 아직 모르겠어요'}</Text></Pressable>
    </>}
    {save.isError && <Text accessibilityRole="alert" style={{ color: '#F75B2B' }}>{apiErrorText(save.error)}</Text>}
  </View>;
}
