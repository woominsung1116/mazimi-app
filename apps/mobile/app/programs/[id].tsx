import React, { useRef, useState } from 'react';
import { Image, Linking, ScrollView, StyleSheet, View } from 'react-native';
import Text from '@/components/Text';
import Tappable from '@/components/Tappable';
import ApplicationResourcesCard from '@/components/ApplicationResourcesCard';
import { ApplicationProcedureBody, stageAccessibilityLabel } from '@/components/ApplicationProcedure';
import BenefitSummarySheet from '@/components/BenefitSummarySheet';
import MyDocumentItems from '@/components/MyDocumentItems';
import { conditionStatus, documentStatus, type RowMark } from '@/lib/detailStatus';
import { useVaultDocuments } from '@/lib/vault';
import { useConfirmDialog } from '@/components/ConfirmDialog';
import UndoToast, { useUndoToast } from '@/components/UndoToast';
import { useLoginPrompt } from '@/components/LoginPromptDialog';
import { ALREADY_SAVED_TOAST, SAVED_UNKNOWN_TOAST, saveDecision, useMemberAction } from '@/components/MemberAction';
import { useLocalSearchParams, useRouter } from 'expo-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { invalidateAfterWrite } from '@/lib/writeEffects';
import { useSafeAreaInsets } from 'react-native-safe-area-context';
import { api, ApiRequestError, displayTitle, type SavedProgram, type ApplicationStage, benefitConditionLinesOf, formatBenefit, summaryLinesOf, hasBenefitAmount, programTypeLabel, resolveApplyUrl } from '@/lib/api';
import { LinkLabel, EmptyState, FigmaIcon, deadlineLabel, ui } from '@/components/FigmaUI';
import { useAuthStore } from '@/store/auth';
import { goBack } from '@/lib/navigation';
import { PROFILE_RETRY_TOAST, useBenefitProfile, useProfileStatus } from '@/lib/useBenefitProfile';
import { enablePush } from '@/lib/notifications';
import { safeOfficialUrl } from '@/lib/assistantTypes';
import { DEADLINE_TONE_COLORS, deadlineTone } from '@/lib/deadline';
import { fieldLabelOf } from '@/constants/fields';
import { categoryStyleOf } from '@/constants/categoryStyle';
import { fig } from '@/constants/theme';
import { alertSheetMessage, GUEST_ALERT_REASON, GUEST_CONDITIONS_REASON, GUEST_DOCUMENT_REASON, GUEST_SAVE_REASON, destinationButtonLabel, documentTextLines } from '@/lib/displayCopy';
import { NOT_FOUND_PROGRAM, errorCopy } from '@/lib/errorCopy';
import { ALREADY_UNSAVED_TOAST, savedAfterToggle, setBookmarkToIntent } from '@/lib/bookmarkSync';
import { REGION_UNKNOWN_LABEL, applicationMethodText, programRegionsLabel } from '@/constants/displayLabels';

/** 자격 조건's region line: none for a school-scoped campus offer; "전국" for an empty (nationwide) list. */
function regionLine(p: { regions: string[] | null; program_type: string }) {
  if (p.program_type === 'campus_benefit') return null;
  if (!p.regions) return '지역 제한은 공식 공고에서 확인해 주세요.';
  const label = programRegionsLabel(p);
  return label ? `지역: ${label}` : REGION_UNKNOWN_LABEL;
}

const HERO_HEIGHT = 281;
/** Sticky top bar height below the safe area (paddingTop 8 + 44 button + paddingBottom 8). */
const TOP_BAR_HEIGHT = 60;
export default function ProgramDetail() {
  const { id } = useLocalSearchParams<{ id: string }>(); const router = useRouter(); const insets = useSafeAreaInsets();
  const user = useAuthStore(s => s.user); const client = useQueryClient(); const scroll = useRef<ScrollView>(null);
  const benefitProfile = useBenefitProfile();
  const profileStatus = useProfileStatus();
  const login = useLoginPrompt();
  // Only a guest gets the login dialog; a restoring member's tap runs once the session is back.
  const toast = useUndoToast();
  const confirm = useConfirmDialog();
  const [summaryOpen, setSummaryOpen] = useState(false);
  // F6.8: the top bar stays; past the hero it turns solid and names the benefit.
  const [pastHero, setPastHero] = useState(false);
  // 조건이 있으면 바로 수정하고, 없으면 처음 입력하는 흐름으로 보낸다
  const editConditions = () => { if (benefitProfile) router.push('/profile-edit'); else member.run('conditions', () => login.prompt(GUEST_CONDITIONS_REASON)); };
  const positions = useRef<Record<string, number>>({}); const [selected, setSelected] = useState('지원 정보');
  const [message, setMessage] = useState('');
  const query = useQuery({ queryKey: ['program', id], queryFn: () => api.getProgram(id), enabled: !!id, retry: (count, error) => !(error instanceof ApiRequestError && error.status === 404) && count < 2 });
  const saved = useQuery({ queryKey: ['saved', user?.id], queryFn: api.getSaved, enabled: !!user });
  // The tap's intent travels with the request: PUT sets it (idempotent, so a retry
  // after a lost response is safe); an older server's toggle is checked against a
  // fresh read before any second toggle. A save tap can never delete a bookmark.
  const bookmark = useMutation({ mutationFn: (want: boolean) => setBookmarkToIntent({
    put: bookmarked => api.setBookmark(id, bookmarked),
    toggle: () => api.toggleBookmark(id),
    isSaved: async () => (await api.getSaved()).items.some(item => item.id === id),
  }, want), onSuccess: (result, want) => {
    if (result.bookmarked !== want) { toast.show({ text: SAVED_UNKNOWN_TOAST }); void invalidateAfterWrite(client, 'bookmark', { userId: user?.id, programId: id }); return; }
    if (result.corrected) toast.show({ text: want ? ALREADY_SAVED_TOAST : ALREADY_UNSAVED_TOAST });
    // The icon follows the server's (last) answer at once, so a quick second tap is decided on the new state.
    const row = query.data ? { ...query.data, available: true, effective_deadline: query.data.effective_deadline ?? null } as SavedProgram : undefined;
    client.setQueryData<{ items: SavedProgram[] }>(['saved', user?.id], old => savedAfterToggle(old, id, row, result.bookmarked));
    void invalidateAfterWrite(client, 'bookmark', { userId: user?.id, programId: id });
  }, onError: (e: Error) => {
    toast.show({ text: `관심 혜택을 저장하지 못했어요. ${errorCopy(e)}` });
    // The request may have reached the server: the next tap decides on server truth, not this cache.
    void invalidateAfterWrite(client, 'bookmark', { userId: user?.id, programId: id });
  } });
  // 혜택별 마감 알림은 서버 상태를 기준으로 보여 준다
  const alertKey = ['alert-preference', user?.id, id];
  const alertPreference = useQuery({ queryKey: alertKey, queryFn: () => api.getAlertPreference(id), enabled: !!user && !!id });
  const alertOn = alertPreference.data?.enabled ?? false;
  // F6.4: a toast says what changed and offers 실행 취소 (the undo is the opposite toggle).
  const alerts = useMutation({ mutationFn: (enabled: boolean) => api.upsertAlertPreference(id, enabled), onSuccess: (_, enabled) => {
    client.setQueryData(alertKey, { program_id: id, enabled });
    void invalidateAfterWrite(client, 'set_alert', { userId: user?.id, programId: id });
    const undo = () => alerts.mutateAsync(!enabled);
    toast.show({ text: enabled ? '마감 알림을 켰어요' : '마감 알림을 껐어요', undo });
    // Push permission off: same toast, same undo, plus why no push will come.
    if (enabled) void enablePush().then(granted => { if (!granted) toast.show({ text: '마감 알림을 켰어요 · 기기 알림 권한이 꺼져 있어 푸시는 오지 않아요', undo }); });
  }, onError: (e: Error) => toast.show({ text: `알림 설정을 바꾸지 못했어요. ${errorCopy(e)}` }) });
  // 34065:11030 / 11091 row marks: the member's per-condition checks (shared with 내 조건·남은 준비 확인) and this device's vault.
  const guidance = useQuery({ queryKey: ['benefit-assistant', user?.id, id], queryFn: ({ signal }) => api.getBenefitGuidance(id, signal), enabled: !!user && !!id, retry: false });
  const vault = useVaultDocuments();
  const agent = useQuery({ queryKey: ['agent-status', user?.id], queryFn: ({ signal }) => api.getAgentStatus(signal), enabled: !!user, staleTime: 60_000, retry: false });
  const progress = useQuery({ queryKey: ['program-progress', user?.id, id], queryFn: () => api.getProgramProgress(id), enabled: !!user && !!id, retry: false });
  const p = query.data;
  async function official() { const url = p ? resolveApplyUrl(p) : null; if (!url) { setMessage('확인된 공식 안내 링크가 없어요. 신청 안내에서 확인해 주세요.'); return; } try { await Linking.openURL(url); } catch { setMessage('공식 안내를 열지 못했어요.'); } }
  // A corporate offer's "전체 조건 확인" opens the official page its text was read from.
  async function sourcePage() { const url = p ? safeOfficialUrl(p.official_url) : null; if (!url) { setMessage('확인된 공식 페이지 링크가 없어요.'); return; } try { await Linking.openURL(url); } catch { setMessage('공식 페이지를 열지 못했어요.'); } }
  function application() { router.push(`/apply-assistant?programId=${id}`); }
  // The hero is a light category tile now (2026-10-31), not the photo of 34065:10919: the icons keep their on-white colours.
  const topBar = (title?: string) => <View style={[styles.topBar, { paddingTop: insets.top + 8 }, pastHero && styles.topBarSolid]}>
    <Tappable accessibilityRole="button" accessibilityLabel="뒤로 가기" onPress={() => goBack(router)} style={styles.topButton}><FigmaIcon name="back" size={20} color={fig.color.primaryBright} /></Tappable>
    <Text numberOfLines={1} accessibilityElementsHidden={!pastHero} importantForAccessibility={pastHero ? 'auto' : 'no-hide-descendants'} style={[styles.topTitle, !pastHero && { opacity: 0 }]}>{title ?? ''}</Text>
    {p ? <Tappable accessibilityRole="button" accessibilityLabel={isSaved ? '관심 혜택 해제' : '관심 혜택 저장'} accessibilityState={{ selected: !!isSaved }} loading={bookmark.isPending || member.waiting === 'save'} onPress={() => member.run('save', () => login.prompt(GUEST_SAVE_REASON), !isSaved)} style={styles.topButton}><FigmaIcon name="bookmarkMono" size={20} color={isSaved ? fig.color.primary : fig.color.grayC8} /></Tappable> : <View style={{ width: 44 }} />}
  </View>;
  const isSaved = saved.data?.items.some(item => item.id === id);
  // 찜 is a server toggle: a tap's intent ("saved" / "not saved") is applied only
  // once the saved list has loaded, and only if it is not already true.
  const saveOnce = (want: unknown) => {
    const decision = saveDecision(want === true, !!isSaved, saved.isError);
    if (decision === 'toggle') bookmark.mutate(want === true);
    else if (decision === 'already-saved') toast.show({ text: ALREADY_SAVED_TOAST });
    else if (decision === 'retry') { toast.show({ text: SAVED_UNKNOWN_TOAST }); void saved.refetch(); }
  };
  const member = useMemberAction({
    save: { run: saveOnce, ready: saved.isSuccess || saved.isError },
    // Turning the alert on asks first with the real alert days (34065:12476) when the notice has a deadline date.
    alert: { run: want => { const message = want === true ? alertSheetMessage(query.data?.deadline_date) : null; if (message) confirm.ask({ title: '마감일 알림 설정', message, confirm: '알림 받기', tone: 'primary', onConfirm: () => alerts.mutate(true) }); else alerts.mutate(want === true); }, ready: alertPreference.isFetched },
    conditions: { run: () => {
      // A failed read is "try again", never a detour to onboarding.
      if (profileStatus.state === 'error') { toast.show({ text: PROFILE_RETRY_TOAST }); profileStatus.retry(); return; }
      router.push(benefitProfile ? '/profile-edit' : '/onboarding?mode=edit');
    }, ready: profileStatus.state !== 'loading' },
  }, { onUnconfirmed: text => toast.show({ text }) });
  // 404: no such programme, or it closed or was retired (the API cannot tell which; a saved or notified one stays listed elsewhere); not a failure to retry.
  const gone = query.error instanceof ApiRequestError && query.error.status === 404;
  if (!p) return <View style={ui.screen}><View style={{ height: insets.top + 60 }} />{gone
    ? <EmptyState title={NOT_FOUND_PROGRAM} description="관심 혜택으로 저장했다면 목록에 그대로 남아 있어요." action="다른 혜택 보기" onAction={() => router.replace('/explore')} />
    : <EmptyState loading={query.isLoading} title={query.isError ? '혜택 정보를 불러오지 못했어요' : '혜택을 불러오고 있어요'} description={query.isError ? errorCopy(query.error) : undefined} action={query.isError ? '다시 시도' : undefined} onAction={() => query.refetch()} />}{topBar()}</View>;
  const docs = p.required_documents ?? [];
  const regionText = regionLine(p);
  const ageText = p.min_age != null || p.max_age != null ? `연령: ${p.min_age != null ? `만 ${p.min_age}세 이상` : ''}${p.min_age != null && p.max_age != null ? ' · ' : ''}${p.max_age != null ? `만 ${p.max_age}세 이하` : ''}` : null;
  const benefitConditionLines = benefitConditionLinesOf(p);
  const conditions = [ageText, regionText, ...benefitConditionLines].filter((line): line is string => !!line);
  const checked = user ? conditionStatus(guidance.data?.eligibility?.checks) : null;
  const docRows = user && docs.length ? documentStatus(docs.map(d => ({ name: d.document_name, detail: d.description })), vault) : null;
  const cleanSummary = summaryLinesOf(p);
  const summaryLines = cleanSummary.length ? cleanSummary : ['공식 안내에서 지원 내용을 확인해 주세요.'];
  const applicationText = applicationMethodText(p.application_method) || '공식 신청처의 안내에 따라 신청해 주세요.';
  const corporate = p.program_type === 'corporate_benefit';
  // 신청 진행 단계 34065:10997: a member's own stages; until they arrive (or if they cannot), the same stages with none highlighted, never a guest's reset.
  const stages: ApplicationStage[] = user ? progress.data?.stages ?? (p.application_progress ?? []).map(stage => ({ ...stage, state: 'upcoming' as const })) : p.application_progress ?? [];
  const urgent = deadlineTone(p) === 'urgent';
  const hero = categoryStyleOf(p);
  // Figma section header 34065:10988: title, optional right note, and the round chevron when it leads somewhere.
  const sectionHeader = (title: string, note?: string, action?: { label: string; onPress: () => void }, weight: '600' | '700' = '700', count = false) => <View style={styles.sectionHeader}>
    <Text style={[styles.heading, { flex: 1, fontWeight: weight }]}>{title}</Text>
    {note ? <Text style={[styles.sectionNote, count && { color: C.primary }]}>{note}</Text> : null}
    {action ? <Tappable accessibilityRole="button" accessibilityLabel={action.label} hitSlop={12} onPress={action.onPress} style={ui.sectionChevron}><FigmaIcon name="arrow" size={12} color={C.primary} /></Tappable> : null}
  </View>;
  // Row marks 34065:11034 (pass, Primary + check), 34065:11066 (fail, Orange + x), 34065:11058 (unknown, Secondary 32% + minus).
  const mark = (state: RowMark) => <View style={[styles.mark, state === 'pass' && { backgroundColor: C.primary }, state === 'fail' && { backgroundColor: C.orange }]}><FigmaIcon name={state === 'pass' ? 'check' : state === 'fail' ? 'close' : 'minus'} size={12} color={C.white} /></View>;
  return <View style={ui.screen}>
    <ScrollView ref={scroll} contentContainerStyle={{ paddingBottom: 12 }} showsVerticalScrollIndicator={false} scrollEventThrottle={32} onScroll={e => { const past = e.nativeEvent.contentOffset.y > HERO_HEIGHT - 80 - insets.top; if (past !== pastHero) setPastHero(past); }}>
      {/* Hero: the category's 3D image on its tile colour (user decision 2026-10-31; was one room photo for every programme) */}
      <View style={{ width: '100%', height: HERO_HEIGHT, backgroundColor: hero.tile, alignItems: 'center', justifyContent: 'center', paddingBottom: 20 }} accessibilityElementsHidden importantForAccessibility="no-hide-descendants"><Image source={hero.hero} style={{ width: 180, height: 180 }} resizeMode="contain" /></View>
      <View style={styles.sheet}>
        <View style={styles.grip} />
        {/* 34065:10924 summary */}
        <View style={styles.summary}>
          <View style={{ gap: 4 }}>
            <View style={ui.row}>
              <View style={styles.category}><Text style={styles.categoryText}>{fieldLabelOf(p, programTypeLabel(p.program_type))}</Text></View>
              <Text numberOfLines={1} style={styles.providerText}>{p.provider_name || p.company_name}</Text>
            </View>
            <Text style={styles.title}>{displayTitle(p)}</Text>
          </View>
          <ApplicationResourcesCard programId={id} policy={p.application_policy} programType={p.program_type} />
          <View style={{ gap: 8 }}>{[['운영', p.provider_name || p.company_name || '공식 안내 확인'], ['기간', deadlineLabel(p)], ['나이', ageText ?? '공식 공고 확인']].map(([label, value], index) => <View key={label} style={styles.meta}><Text style={styles.metaLabel}>{label}</Text><Text style={[styles.metaValue, index === 0 && { color: C.black }, label === '기간' && urgent && { color: DEADLINE_TONE_COLORS.urgent.text, fontWeight: '600' }]}>{value}</Text></View>)}</View>
          {/* F5.5: only a stated amount; no "혜택 확인" placeholder */}
          {hasBenefitAmount(p) ? <View style={styles.amountRow}><Text style={styles.heading}>지원 혜택</Text><Text style={styles.amount}>{formatBenefit(p)}</Text></View> : null}
        </View>
      </View>
      <View style={styles.tabs}>{['지원 정보', '진행 단계', '자격 조건', '필요 서류'].map(label => <Tappable key={label} accessibilityRole="tab" accessibilityState={{ selected: selected === label }} onPress={() => { setSelected(label); scroll.current?.scrollTo({ y: Math.max(0, (positions.current[label] ?? 500) - insets.top - TOP_BAR_HEIGHT), animated: true }); }} style={[styles.tab, selected === label && styles.tabSelected]}><Text style={[styles.tabText, selected === label && styles.tabTextSelected]}>{label}</Text></Tappable>)}</View>
      <View onLayout={e => { positions.current['지원 정보'] = e.nativeEvent.layout.y; }} style={styles.section}>
        {sectionHeader('지원 정보')}
        <View style={{ gap: 8 }}>{summaryLines.map((line, index) => <View key={index} style={{ flexDirection: 'row', gap: 8 }}>{summaryLines.length > 1 && <View style={styles.bullet} />}<Text style={[styles.body, { flex: 1 }]}>{line}</Text></View>)}</View>
        {/* F5.7: 혜택 요약 opens a sheet; the official link sits at its bottom (34065:10984) */}
        <Tappable accessibilityRole="button" hitSlop={3} onPress={() => setSummaryOpen(true)} style={styles.outline}><Text style={styles.small}>혜택 요약</Text></Tappable>
      </View>
      {/* 34065:10986: the stepper; its chevron opens the agent when it is on, the application helper otherwise */}
      <View onLayout={e => { positions.current['진행 단계'] = e.nativeEvent.layout.y; }} style={styles.section}>
        {sectionHeader('신청 진행 단계', undefined, agent.data?.enabled ? { label: '이 혜택 비서에게 맡기기', onPress: () => router.push(`/agent?programId=${id}`) } : { label: '신청 준비하기', onPress: application }, '600')}
        {stages.length ? <View style={styles.stepper}>
          <View style={styles.stepRow}>{stages.map((stage, index) => <React.Fragment key={stage.stage}>{index > 0 && <View style={[styles.connector, { backgroundColor: stage.state === 'upcoming' ? C.secondary8 : C.secondary32 }]} />}<View accessible accessibilityLabel={stageAccessibilityLabel(stage, index)} style={[styles.step, { backgroundColor: STAGE_DOT[stage.state] }]}><Text style={styles.stepNumber}>{index + 1}</Text></View></React.Fragment>)}</View>
          <View style={styles.stepLabels}>{stages.map(stage => <Text key={stage.stage} style={[styles.stepLabel, { color: STAGE_LABEL[stage.state] }]}>{stage.label}</Text>)}</View>
        </View> : null}
      </View>
      {/* 34065:11019: the chevron opens 내 조건·남은 준비 확인 */}
      <View onLayout={e => { positions.current['자격 조건'] = e.nativeEvent.layout.y; }} style={styles.section}>
        {sectionHeader('자격 조건', checked?.count ?? '확인 필요', { label: '내 조건·남은 준비 확인', onPress: () => router.push(`/benefit-assistant?programId=${id}`) }, '700', !!checked)}
        <View style={styles.box}>
          {(checked?.rows ?? conditions.map(title => ({ title, note: null, mark: 'unknown' as RowMark }))).map((row, index, rows) => <View key={index} style={[styles.item, index === rows.length - 1 && styles.itemLast]}>{mark(row.mark)}<View style={{ flex: 1, gap: 4 }}><Text style={styles.itemText}>{row.title}</Text>{row.note ? <Text style={styles.itemNote}>{row.note}</Text> : null}</View></View>)}
          <Tappable accessibilityRole="button" hitSlop={3} onPress={() => editConditions()} style={styles.outline}><Text style={styles.small}>자격 조건 수정</Text></Tappable>
        </View>
      </View>
      <View onLayout={e => { positions.current['필요 서류'] = e.nativeEvent.layout.y; }} style={styles.section}>
        {sectionHeader('필요 서류', docRows?.count ?? undefined, { label: '서류 관리', onPress: () => router.push('/document-vault') }, '700', true)}
        <View style={styles.box}>
          {docs.length ? docs.map((doc, index) => { const row = docRows?.rows[index]; return <View key={doc.id} style={[styles.item, index === docs.length - 1 && styles.itemLast]}>{mark(row?.mark ?? 'unknown')}<View style={{ flex: 1, gap: 4 }}><Text style={styles.itemText}>{doc.document_name}{doc.is_required === false ? ' (해당 시)' : ''}</Text>{doc.description && <Text style={styles.itemNote}>{doc.description}</Text>}{row?.note ? <Text style={styles.itemNote}>{row.note}</Text> : null}</View></View>; }) : <Text style={styles.body}>{documentTextLines(p).join('\n') || '공식 신청처에서 제출 대상 여부와 필요 서류를 확인해 주세요.'}</Text>}
          {(p.required_document_notes ?? []).map((note, index) => <Text key={index} style={ui.muted}>※ {note}</Text>)}
          {/* F6.2: what the notice does not list, the user adds */}
          <MyDocumentItems programId={id} inset={0} onGuest={() => login.prompt(GUEST_DOCUMENT_REASON)} />
          <Tappable accessibilityRole="button" hitSlop={3} onPress={() => router.push('/document-vault')} style={styles.outline}><Text style={styles.small}>서류 관리</Text></Tappable>
        </View>
      </View>
      <View style={styles.section}>
        {sectionHeader('신청 방법', undefined, { label: destinationButtonLabel(p.application_policy?.destination?.label), onPress: application })}
        <ApplicationProcedureBody procedure={p.application_procedure} fallback={applicationText} />
        <Tappable accessibilityRole="button" hitSlop={3} onPress={application} style={styles.outline}><Text style={styles.small}>{destinationButtonLabel(p.application_policy?.destination?.label)}</Text></Tappable>
        {!!message && <Text accessibilityRole="alert" style={ui.muted}>{message}</Text>}
      </View>
    </ScrollView>
    {topBar(displayTitle(p))}
    <UndoToast toast={toast.toast} onHide={toast.hide} style={{ bottom: Math.max(insets.bottom, 12) + 76 }} />
    <View style={[styles.bottom, { paddingBottom: Math.max(insets.bottom, 12) }]}><Tappable accessibilityRole="switch" accessibilityLabel="마감 알림" accessibilityState={{ checked: alertOn }} disabled={!!user && alertPreference.isLoading} loading={alerts.isPending || member.waiting === 'alert'} onPress={() => member.run('alert', () => login.prompt(GUEST_ALERT_REASON), !alertOn)} style={[styles.bell, alertOn && styles.bellOn]}><FigmaIcon name="bell" size={24} color={alertOn ? fig.color.primary : fig.color.grayE1} /><View pointerEvents="none" style={styles.bellTip}><Text numberOfLines={1} style={styles.bellTipText}>마감 알림</Text></View></Tappable><Tappable accessibilityRole="button" onPress={() => router.push(`/apply-assistant?programId=${id}`)} style={[ui.primaryButton, styles.cta]} feedback="opacity"><Text style={ui.primaryButtonText}>{p.application_policy?.workflow.no_application_required ? "혜택 이용 조건 확인" : "신청 준비하기"}</Text></Tappable></View>
    <BenefitSummarySheet program={p} visible={summaryOpen} onClose={() => setSummaryOpen(false)} conditions={conditions} applicationText={applicationText} onOfficial={() => { setSummaryOpen(false); return corporate ? sourcePage() : official(); }} />
    {login.dialog}
    {confirm.dialog}
  </View>;
}
// Figma Detail 34065:10914: summary 34065:10924, tabs 34065:10958, sections 34065:10967, bottom bar.
const C = fig.color, K = fig.component, T = fig.text;
// 34065:10999/11003/11007 dots and 11015-11018 labels: done, current, upcoming.
const STAGE_DOT = { done: C.deepGray, current: C.primary, upcoming: C.grayE1 } as const;
const STAGE_LABEL = { done: C.black, current: C.primary, upcoming: C.deepGray } as const;
const styles = StyleSheet.create({
  // Photo header 34065:11165
  topBar: { position: 'absolute', top: 0, left: 0, right: 0, paddingHorizontal: 4, paddingBottom: 6, flexDirection: 'row', alignItems: 'center', gap: 8 }, topBarSolid: { backgroundColor: K.floatingHeader.backgroundColor, borderBottomWidth: 1, borderBottomColor: C.grayF1 },
  topButton: { width: 44, height: 44, alignItems: 'center', justifyContent: 'center' }, topTitle: { flex: 1, ...T.bold16, color: C.black, textAlign: 'center' },
  // Sheet 34065:10920 and summary 34065:10924
  sheet: { marginTop: -20, backgroundColor: C.white, borderTopLeftRadius: K.sheet.radius, borderTopRightRadius: K.sheet.radius, paddingTop: 12 }, grip: { width: K.sheet.handleWidth, height: K.sheet.handleHeight, borderRadius: 100, backgroundColor: C.grayE8, alignSelf: 'center', marginBottom: 16 },
  summary: { paddingHorizontal: 16, paddingTop: 12, paddingBottom: 16, gap: 12, borderBottomWidth: 4, borderBottomColor: C.grayF7 },
  category: { paddingHorizontal: K.badge.paddingHorizontal, paddingVertical: K.badge.paddingVertical, borderRadius: K.badge.radius, backgroundColor: C.primary, boxShadow: fig.shadow.badgeInner }, categoryText: { ...T.badge11, color: C.white },
  providerText: { ...T.semibold12, color: C.deepGray, marginLeft: 'auto', maxWidth: 190 },
  title: { ...T.bold18, color: C.title },
  meta: { flexDirection: 'row', gap: 12 }, metaLabel: { width: 80, ...T.body13, color: C.deepGray }, metaValue: { flex: 1, ...T.body13, color: C.gray66 },
  amountRow: { flexDirection: 'row', alignItems: 'flex-end', justifyContent: 'space-between' }, amount: { ...T.bold20, color: C.secondary },
  heading: { ...T.bold14, color: C.black },
  // Tabs 34065:10958
  tabs: { flexDirection: 'row' }, tab: { flex: 1, height: 46, alignItems: 'center', justifyContent: 'center', borderBottomWidth: 1, borderBottomColor: C.secondary8 }, tabSelected: { borderBottomWidth: 2, borderBottomColor: C.black },
  tabText: { ...T.body12, color: C.gray66 }, tabTextSelected: { ...T.semibold12, color: C.black },
  // Sections 34065:10967 / 34065:11019
  section: { padding: 16, gap: 8, borderBottomWidth: 4, borderBottomColor: C.grayF7 },
  sectionHeader: { flexDirection: 'row', alignItems: 'center', gap: 8, minHeight: 28 }, sectionNote: { ...T.semibold12, color: C.deepGray },
  body: { ...T.body13, color: C.gray66 }, bullet: { width: 4, height: 4, borderRadius: 2, backgroundColor: C.secondary32, marginTop: 8 },
  outline: { height: 38, borderWidth: 1, borderColor: C.grayE8, backgroundColor: C.white, borderRadius: K.badge.radius, alignItems: 'center', justifyContent: 'center', marginTop: 4 }, small: { ...T.bold12, color: C.gray66 },
  box: { borderWidth: 1, borderColor: C.grayF1, borderRadius: 12, padding: 16, gap: 16, backgroundColor: C.white },
  item: { flexDirection: 'row', gap: 8, paddingBottom: 12, borderBottomWidth: 1, borderColor: C.grayF1 }, itemLast: { borderBottomWidth: 0, paddingBottom: 0 },
  mark: { width: 16, height: 16, borderRadius: 10, padding: 2, backgroundColor: C.secondary32, marginTop: 2, alignItems: 'center', justifyContent: 'center' }, // 34065:11058
  itemText: { ...T.semibold13, color: C.black, flexShrink: 1 }, itemNote: { ...T.body11, color: C.gray66 },
  // Stepper 34065:10997
  stepper: { borderWidth: 1, borderColor: C.grayF1, borderRadius: 12, backgroundColor: C.white, paddingTop: 16, paddingHorizontal: 16, paddingBottom: 8, gap: 12 },
  stepRow: { flexDirection: 'row', alignItems: 'center', paddingHorizontal: 24, gap: 2 }, step: { width: 20, height: 20, borderRadius: 10, alignItems: 'center', justifyContent: 'center' }, stepNumber: { ...T.badge10, color: C.white, textAlign: 'center' },
  connector: { flex: 1, height: 1, borderRadius: 1 },
  stepLabels: { flexDirection: 'row', gap: 12 }, stepLabel: { flex: 1, ...T.semibold12, textAlign: 'center' },
  // Bottom bar 34065:11178
  cta: { flex: 1, backgroundColor: C.navy, boxShadow: fig.shadow.card },
  bottom: { flexDirection: 'row', alignItems: 'center', gap: 8, paddingHorizontal: 16, paddingTop: 8, borderTopWidth: 1, borderColor: C.secondary4, backgroundColor: C.white96, borderTopLeftRadius: 16, borderTopRightRadius: 16, boxShadow: fig.shadow.bar },
  bell: { width: 52, height: 52, borderWidth: 1, borderColor: C.grayE8, borderRadius: 12, alignItems: 'center', justifyContent: 'center', backgroundColor: C.white, boxShadow: fig.shadow.badgeInner }, bellOn: { borderColor: C.primary, backgroundColor: C.secondary4 },
  bellTip: { position: 'absolute', top: -24, left: -6, width: 65, height: 28, paddingHorizontal: 8, borderRadius: 12, backgroundColor: C.navy, justifyContent: 'center' }, bellTipText: { ...T.semibold13, color: C.white }, // 34065:11183
});
