import React, { useState } from 'react';
import { Linking, Platform, Pressable, View } from 'react-native';
import Text from '@/components/Text';
import { ui } from '@/components/FigmaUI';
import { api, type ApiProgram, type ApplicationResource } from '@/lib/api';
import { currentResource, resourceMetaLine } from '@/lib/applicationResources';
import { vaultFilesSupported } from '@/lib/vault';
import { saveOfficialForm, VaultSaveError } from '@/lib/vaultSave';
import { requireLogin } from '@/lib/navigation';
import { confirmAction, notify } from '@/lib/notify';
import { useAuthStore } from '@/store/auth';
import { useQueryClient } from '@tanstack/react-query';
import { useRouter } from 'expo-router';

export default function ApplicationResourcesCard({programId, policy, programType}: {programId: string; policy: ApiProgram['application_policy']; programType?: string}) {
  const router = useRouter();
  // 서류 작성 도우미 is never offered for financial-product applications.
  const canAssist = programType !== 'financial_product';
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const userId = useAuthStore(s => s.user?.id);
  const client = useQueryClient();
  // Web keeps no files in the vault (no encrypted storage there), so the action is native-only.
  const canStore = vaultFilesSupported();
  const resources = policy?.resources;
  async function open(resource: ApplicationResource) {
    if (busy) return;
    const popup = Platform.OS === 'web' ? window.open('about:blank', '_blank') : null;
    if (popup) popup.opener = null;
    setBusy(true); setError('');
    try {
      const latest = await api.getProgram(programId);
      const fresh = currentResource(latest, resource);
      if (popup) popup.location.replace(fresh.url);
      else await Linking.openURL(fresh.url);
    } catch (e) {
      popup?.close(); setError(e instanceof Error ? e.message : '공식 자료를 열지 못했어요.');
    } finally { setBusy(false); }
  }
  // The reviewed official file, re-read now, checked against its content_hash and stored encrypted on this device.
  async function store(resource: ApplicationResource) {
    if (busy) return;
    if (!userId) { requireLogin(router, '서류를 보관하려면 로그인해 주세요.'); return; }
    setBusy(true); setError('');
    let fresh: ApplicationResource | null = null;
    try {
      const latest = await api.getProgram(programId);
      fresh = currentResource(latest, resource);
      await saveOfficialForm(userId, { id: programId, title: latest.title }, fresh);
      void client.invalidateQueries({ queryKey: ['vault-documents', userId] });
      notify('보관함에 넣었어요', '서류 보관함의 기타에서 볼 수 있어요. 제출은 기관에 직접 해 주세요.');
    } catch (e) {
      const official = fresh;
      if (e instanceof VaultSaveError && e.problem === 'duplicate') notify('이미 보관함에 있어요', '서류 보관함의 기타에서 볼 수 있어요.');
      else if (e instanceof VaultSaveError && official && ['unverified', 'format', 'too_large'].includes(e.problem)) confirmAction('보관함에 넣지 않았어요', e.message, '공식 양식 받기', () => void open(official));
      else setError(e instanceof Error ? e.message : '보관함에 저장하지 못했어요.');
    } finally { setBusy(false); }
  }
  // Only what the person can act on: the notice and the blank forms. The
  // pipeline's review status is never shown.
  if (!resources || (!resources.notice && !resources.templates.length)) return null;
  return <View style={[ui.benefitCard, {gap: 12}]}>
    <Text style={ui.cardTitle}>공식 신청처 · 제출 양식</Text>
    {resources.notice && <Pressable disabled={busy} accessibilityRole="button" onPress={() => void open(resources.notice!)} style={ui.outlineButton}><Text style={ui.outlineButtonText}>공식 공고 보기</Text></Pressable>}
    {resources.templates.map(resource => <View key={resource.id} style={{gap:8}}>
      <Text style={ui.cardTitle}>{resource.label}</Text>
      <Text style={ui.muted}>{resourceMetaLine(resource)}</Text>
      <Pressable disabled={busy} accessibilityRole="button" accessibilityLabel={`공식 양식 받기 ${resource.label}`} onPress={() => void open(resource)} style={ui.outlineButton}><Text style={ui.outlineButtonText}>공식 양식 받기</Text></Pressable>
      {canStore && <Pressable disabled={busy} accessibilityRole="button" accessibilityLabel={`서류 보관함에 넣기 ${resource.label}`} onPress={() => void store(resource)} style={ui.outlineButton}><Text style={ui.outlineButtonText}>서류 보관함에 넣기</Text></Pressable>}
      {canAssist && <Pressable disabled={busy} accessibilityRole="button" accessibilityLabel={`서류 작성 도우미 ${resource.label}`} onPress={() => router.push({ pathname: '/form-fill', params: { programId, resourceId: resource.id } })} style={[ui.outlineButton, {borderColor: '#34BB84', backgroundColor: '#EFFFF8'}]}><Text style={ui.outlineButtonText}>서류 작성 도우미로 채우기</Text></Pressable>}
    </View>)}
    {!!error && <Text accessibilityRole="alert" style={ui.muted}>{error}</Text>}
  </View>;
}
