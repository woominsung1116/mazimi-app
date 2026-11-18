import React, { useState } from 'react';
import { Pressable, ScrollView, View, StyleSheet, Linking, Platform } from 'react-native';
import { useLocalSearchParams, useRouter } from 'expo-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useSafeAreaInsets } from 'react-native-safe-area-context';
import Text from '@/components/Text';
import { CheckMark, EmptyState, LinkLabel, ScreenHeader, ui } from '@/components/FigmaUI';
import ApplicationJourneyCard from '@/components/ApplicationJourneyCard';
import ApplicationResourcesCard from '@/components/ApplicationResourcesCard';
import { destinationForChannel, handoffLabel } from '@/lib/applicationResources';
import { api } from '@/lib/api';
import { invalidateAfterWrite } from '@/lib/writeEffects';
import { NOT_FOUND_PROGRAM, apiErrorText } from '@/lib/errorCopy';
import { useAuthStore } from '@/store/auth';
import { useVaultDocuments } from '@/lib/vault';
import { documentReadiness, readinessLabels, readinessNote } from '@/lib/documentReadiness';
import { rememberApplication } from '@/lib/pendingApplication';
import NonMemberState from '@/components/NonMemberState';
import { FINAL_CHECK_LINE } from '@/lib/displayCopy';

/** Host of an HTTPS link the app may open, or null. Mirrors the server's public-host rule. */
function safeHttpsHost(value: string): string | null {
  try {
    const url = new URL(value);
    const host = url.hostname.toLowerCase();
    const isIp = /^[\d.]+$/.test(host) || host.includes(':');
    return url.protocol === 'https:' && !url.username && !url.password && (!url.port || url.port === '443') && host.includes('.') && !isIp && host !== 'localhost' && !host.endsWith('.localhost') ? host : null;
  } catch { return null; }
}

/** All program categories share a server-versioned, resumable preparation plan. This screen assists the user's own input; it never fills or submits a form. */
export default function ApplyAssistant() {
  const { programId } = useLocalSearchParams<{ programId: string }>();
  const router = useRouter(); const client = useQueryClient(); const insets = useSafeAreaInsets();
  const user = useAuthStore(s => s.user); const vault = useVaultDocuments();
  const [error, setError] = useState(''); const [starting, setStarting] = useState(false);
  const key = ['preparation', user?.id, programId];
  const plan = useQuery({ queryKey: key, queryFn: () => api.getPreparation(programId), enabled: !!user && !!programId });
  const journeyKey = ['application-journey', user?.id, programId];
  const journey = useQuery({ queryKey: journeyKey, queryFn: () => api.getApplicationJourney(programId), enabled: !!user && !!programId });
  const save = useMutation({ mutationFn: ({ completed, channel }: { completed: string[]; channel?: string }) => api.savePreparation(programId, plan.data!.revision, completed, channel),
    onSuccess: data => { client.setQueryData(key, data); void invalidateAfterWrite(client, 'toggle_checklist_item', { userId: user?.id, programId }); setError(''); },
    onError: (e: Error) => { setError(apiErrorText(e)); void plan.refetch(); } });
  async function openOfficial(informationOnly = false) {
    if (!plan.data?.policy.can_prepare || starting) return;
    const channel = plan.data.selected_channel;
    const infoOnly = informationOnly || plan.data.policy.workflow.no_application_required;
    if (!infoOnly && !channel) { setError('접수 방식을 먼저 선택해 주세요.'); return; }
    const destination = destinationForChannel(plan.data.policy, channel, infoOnly);
    const url = destination?.url;
    // Reserve a browser tab during the click gesture; a delayed API response
    // must not turn navigation into a silently blocked popup.
    const popup = Platform.OS === 'web' && url ? window.open('about:blank', '_blank') : null;
    if (popup) popup.opener = null;
    setStarting(true); setError('');
    try {
      const fresh = await api.getPreparation(programId);
      if (fresh.revision !== plan.data.revision || !fresh.policy.can_prepare) {
        client.setQueryData(key, fresh);
        throw new Error('공고 또는 신청 경로가 변경됐어요. 새 안내를 확인해 주세요.');
      }
      if (!infoOnly) {
        const saved = await api.startApplicationJourney(programId, plan.data.revision, channel!);
        client.setQueryData(journeyKey, { journey: saved });
        void invalidateAfterWrite(client, 'record_application_status', { userId: user?.id, programId });
      }
      if (url) {
        const parsed = new URL(url);
        if (parsed.protocol !== 'https:' || parsed.origin !== destination?.origin) throw new Error('공식 신청 주소를 다시 확인해 주세요.');
        if (popup) popup.location.replace(url);
        else await Linking.openURL(url);
      }
    } catch (e) {
      popup?.close(); setError(e instanceof Error ? e.message : '신청 준비를 저장하지 못했어요.');
      void plan.refetch();
    } finally { setStarting(false); }
  }
  return <View style={ui.screen}><ScreenHeader title="신청 도우미" back />
    {!user ? <NonMemberState title="로그인하고 신청 준비를 시작하세요" description="준비한 항목을 저장하고 다음에 이어서 진행할 수 있어요." onLogin={() => { void rememberApplication(programId).finally(() => router.push('/login?return=1')); }} /> :
    !plan.data ? <ScrollView contentContainerStyle={{padding:16}}><EmptyState loading={plan.isLoading} title={plan.isError ? '신청 정보를 불러오지 못했어요' : '신청에 필요한 항목을 준비하고 있어요'} description={plan.isError ? apiErrorText(plan.error, NOT_FOUND_PROGRAM) : undefined} action={plan.isError ? '다시 시도' : undefined} onAction={() => plan.refetch()} />{journey.data?.journey && <ApplicationJourneyCard key={journey.data.journey.id} journey={journey.data.journey} />}</ScrollView> : <>
      <ScrollView contentContainerStyle={{ padding: 16, paddingBottom: 32 }}>
        <Text style={styles.eyebrow}>나의 신청 준비</Text><Text style={styles.title}>{plan.data.title}</Text>
        <ApplicationResourcesCard programId={programId} policy={plan.data.policy} />
        <View style={styles.progress}><View style={[styles.progressFill, { width: `${plan.data.completed_tasks.length / plan.data.tasks.length * 100}%` }]} /></View>
        <Text style={ui.muted}>{plan.data.completed_tasks.length} / {plan.data.tasks.length}개 확인</Text>
        <View style={styles.policy}><Text style={styles.taskTitle}>{plan.data.policy.workflow.no_application_required ? '이 혜택은 별도 신청이 없다고 안내되어 있어요' : '접수 방식'}</Text>
          {plan.data.policy.workflow.channels.length > 1 ? <Text style={styles.detail}>공고에 나온 방식 중 이용할 방법을 선택해 주세요.</Text> : null}
          {plan.data.policy.workflow.channels.map(channel => <View key={channel.id} style={{marginTop:12}}>
            <Pressable accessibilityRole="radio" accessibilityLabel={channel.label} aria-checked={plan.data!.selected_channel===channel.id} accessibilityState={{checked:plan.data!.selected_channel===channel.id}} disabled={save.isPending} onPress={()=>save.mutate({completed:plan.data!.completed_tasks.filter(id=>id!=='official_process'),channel:channel.id})} style={[ui.outlineButton,plan.data!.selected_channel===channel.id&&{borderColor:'#34BB84',backgroundColor:'#EFFFF8'}]}><Text style={ui.outlineButtonText}>{channel.label}</Text>{plan.data!.selected_channel===channel.id&&<CheckMark />}</Pressable>
          </View>)}
        </View>
        {plan.data.progress_reset && <Text style={styles.notice}>공고 내용이 변경되어 준비 항목을 새로 만들었어요. 다시 확인해 주세요.</Text>}
        {plan.data.tasks.map(task => {
          const done = plan.data!.completed_tasks.includes(task.id);
          // Matched on this device; a match is a convenience, never proof of validity.
          const found = task.kind === 'document' ? documentReadiness([task], vault)[0] : undefined;
          // Documents outside the type catalogue: the same exact-name match as before (re-review).
          const sameName = task.kind === 'document' && (!found || found.status === 'missing') && vault.some(d => d.name.replace(/\s/g, '') === task.title.replace(/\s/g, '') && d.fileUri && (!d.expiresAt || Date.parse(d.expiresAt) > Date.now()));
          return <View key={task.id} style={styles.task}>
            <Pressable disabled={save.isPending} accessibilityRole="checkbox" aria-checked={done} accessibilityState={{ checked: done, disabled: save.isPending }} onPress={() => save.mutate({completed:done ? plan.data!.completed_tasks.filter(id => id !== task.id) : [...plan.data!.completed_tasks, task.id]})} style={styles.taskHeading}>
              <View style={[styles.check, done && styles.checked]}>{done ? <CheckMark color="#FFFFFF" /> : null}</View><Text style={styles.taskTitle}>{task.title}</Text>
            </Pressable><Text style={styles.detail}>{task.detail}</Text>
            {found && found.status !== 'missing' && <Text style={styles.notice}>{readinessLabels[found.status]} · {readinessNote(found)}. 내용을 확인해 주세요.</Text>}
            {sameName && <Text style={styles.notice}>보관함에 같은 이름의 서류가 있어요. 발급일과 내용을 확인해 주세요.</Text>}
            {(task.kind === 'document' || task.kind === 'documents_unknown') && <Pressable onPress={() => router.push('/document-vault')} style={styles.link}><LinkLabel style={styles.linkText}>서류 보관함 열기</LinkLabel></Pressable>}
            {task.kind === 'information' && <Pressable onPress={() => router.push('/onboarding')} style={styles.link}><Text style={styles.linkText}>저장된 프로필 확인하기</Text></Pressable>}
          </View>;
        })}
        <View style={styles.policy}><Text style={styles.taskTitle}>공식 안내에 따라 신청을 이어가세요</Text><Text style={styles.detail}>{destinationForChannel(plan.data.policy, plan.data.selected_channel)?.explanation || FINAL_CHECK_LINE}</Text>
          {plan.data.official_links.map(link => { const host = safeHttpsHost(link.url); if (!host) return null;
            return <View key={link.url} style={{ marginTop: 14, gap: 8 }}><Pressable accessibilityRole="button" accessibilityLabel={`공고에 적힌 링크 열기 ${host}`} onPress={() => { if (Platform.OS === 'web') window.open(link.url, '_blank', 'noopener,noreferrer'); else void Linking.openURL(link.url).catch(() => setError('링크를 열지 못했어요.')); }} style={ui.outlineButton}><Text style={ui.outlineButtonText}>{host} 열기</Text></Pressable></View>; })}
        </View>
        {journey.data?.journey && <ApplicationJourneyCard key={journey.data.journey.id} journey={journey.data.journey} />}
        {journey.isError && <Text style={ui.muted}>이전에 저장한 접수 기록을 불러오지 못했어요. 새로 신청하기 전에 신청 이력을 확인해 주세요.</Text>}
        {!!error && <Text accessibilityRole="alert" style={{ color: '#070428', marginTop: 12 }}>{error}</Text>}
      </ScrollView><View style={{ padding: 16, paddingBottom: Math.max(insets.bottom, 16), borderTopWidth: 1, borderColor: '#F1F1F1' }}><Pressable accessibilityRole="button" disabled={!plan.data.policy.can_prepare || starting || (!plan.data.policy.workflow.no_application_required && !plan.data.selected_channel) || journey.isError || (plan.data.policy.workflow.no_application_required && !plan.data.policy.destination)} onPress={() => journey.data?.journey?.outcome==='reported_applied' ? router.push(`/application/${programId}`) : void openOfficial()} style={[ui.primaryButton, !plan.data.policy.can_prepare && { opacity: 0.4 }]}><Text style={ui.primaryButtonText}>{starting ? '준비 저장 중…' : journey.data?.journey?.outcome==='reported_applied' ? '기록된 신청 확인' : plan.data.policy.workflow.no_application_required ? (plan.data.policy.destination ? '이용 안내 확인' : '별도 신청이 없는 혜택') : handoffLabel(plan.data.policy, plan.data.selected_channel)}</Text></Pressable></View>
    </>}
  </View>;
}
const styles = StyleSheet.create({
  eyebrow: { fontSize: 12, lineHeight: 18, color: '#34BB84', marginTop: 8, marginBottom: 8 }, title: { fontSize: 20, lineHeight: 32, fontWeight: '700', color: '#070428' },
  progress: { height: 5, backgroundColor: '#F7F9FA', borderRadius: 4, marginTop: 24, marginBottom: 10 }, progressFill: { height: 5, backgroundColor: '#34BB84', borderRadius: 4 },
  task: { paddingVertical: 22, borderBottomWidth: 1, borderColor: '#F1F1F1' }, taskHeading: { flexDirection: 'row', alignItems: 'center', gap: 12, minHeight: 36 },
  taskTitle: { fontSize: 14, lineHeight: 20, fontWeight: '700', color: '#070428', flexShrink: 1 }, check: { height: 22, width: 22, borderRadius: 6, borderWidth: 1, borderColor: '#F1F1F1', alignItems: 'center', justifyContent: 'center' }, checked: { backgroundColor: '#34BB84', borderColor: '#34BB84' },
  detail: { fontSize: 13, lineHeight: 20, color: '#5D5D62', marginVertical: 10 }, link: { minHeight: 36, justifyContent: 'center' }, linkText: { color: '#239969', fontSize: 13, lineHeight: 20 }, notice: { fontSize: 12, lineHeight: 18, color: '#239969', marginVertical: 10 }, policy: { backgroundColor: '#F7F9FA', borderRadius: 16, padding: 16, marginTop: 24 },
});
