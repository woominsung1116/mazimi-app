import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ActivityIndicator, Linking, Platform, Pressable, ScrollView, View, StyleSheet, useWindowDimensions, Image } from 'react-native';
import { useFocusEffect, useLocalSearchParams, useRouter } from 'expo-router';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useSafeAreaInsets } from 'react-native-safe-area-context';
import * as ScreenCapture from 'expo-screen-capture';
import Text from '@/components/Text';
import { FigmaToggle, FigmaIcon, CheckMark, EmptyState, ScreenHeader, ui } from '@/components/FigmaUI';
import FieldSheet, { groupHint } from '@/components/formFill/FieldSheet';
import GuidedSheet from '@/components/formFill/GuidedSheet';
import PdfRenderer, { type RenderJob, type RenderedPage } from '@/components/formFill/PdfRenderer';
import { PageWithFields, maskSensitive } from '@/components/formFill/PageView';
import { api, resolveApplyUrl, type ApiProgram, type ApplicationResource } from '@/lib/api';
import { currentResource } from '@/lib/applicationResources';
import { useAuthStore } from '@/store/auth';
import {
  clearSensitive, confirmSuggestion, guidedRows, hasValue, hintFor, markNotApplicable, nextPending, parseTemplate,
  progress, readingOrder, rememberable, sensitiveFilledLabels, setValue, type FieldState, type FormState, type FormTemplate, type GuidedRow, type SuggestionContext, type TemplateField,
} from '@/lib/formFill/fields';
import { analyzePdf, bytesToBase64, chooseMode, DERIVABLE_FORMATS, FieldDrawError, flattenForm, TemplateIntegrityError, templateSource } from '@/lib/formFill/pdf';
import { applicationFormName, programFormName } from '@/lib/documentName';
import { applyChatValues, buildDraft, draftNote, type DraftSummary } from '@/lib/formFill/draft';
import { chatValuesFor, openFormContext, setOpenForm } from '@/lib/formFill/chatFill';
import { vaultFilesSupported } from '@/lib/vault';
import { saveFilledForm, VaultSaveError } from '@/lib/vaultSave';
import { addRecord, clearFormFillCache, downloadVerifiedTemplate, forgetRemembered, loadFormFont, loadRecords, loadRemembered, saveRemembered, shareFilledPdf, type FillRecord } from '@/lib/formFill/device';

type Phase =
  | { kind: 'loading'; message: string }
  | { kind: 'error'; message: string }
  | { kind: 'guided'; reason: string; rows: GuidedRow[] }
  | { kind: 'fill' }
  | { kind: 'review' }
  | { kind: 'done'; records: FillRecord[]; cleared: string[] };

/**
 * 서류 작성 도우미 — 본인 작성 보조. The user fills the official blank form
 * on this device and submits it to the agency themselves. Nothing filled is
 * sent to the server; 주민등록번호·계좌번호 live only in memory until export.
 */
export default function FormFill() {
  // `draft=1` is set by an approved agent card and only adds the draft
  // banner: suggestions are built on every open, as before. `resourceId` is absent
  // when the programme has no fillable form (guided copy sheet only).
  const params = useLocalSearchParams<{ programId: string; resourceId?: string; draft?: string }>();
  // Ids go into API paths and in-app links: UUIDs only (security review L4).
  const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  const programId = typeof params.programId === 'string' && UUID.test(params.programId) ? params.programId : '';
  const resourceId = typeof params.resourceId === 'string' && UUID.test(params.resourceId) ? params.resourceId : undefined;
  const draft = params.draft;
  const router = useRouter(); const insets = useSafeAreaInsets();
  const { width: screenWidth } = useWindowDimensions();
  const user = useAuthStore(s => s.user);
  const userId = user?.id;
  const [phase, setPhase] = useState<Phase>({ kind: 'loading', message: '공식 양식을 확인하고 있어요' });
  const [program, setProgram] = useState<ApiProgram | null>(null);
  const [resource, setResource] = useState<ApplicationResource | null>(null);
  const [template, setTemplate] = useState<FormTemplate | null>(null);
  const [aspects, setAspects] = useState<number[]>([]);
  const [state, setState] = useState<FormState>({});
  const [ctx, setCtx] = useState<SuggestionContext>({ profile: null, remembered: {}, today: new Date() });
  const [pages, setPages] = useState<RenderedPage[] | null>(null);
  const [reviewPages, setReviewPages] = useState<RenderedPage[] | null>(null);
  const [rendererDown, setRendererDown] = useState(false);
  const [job, setJob] = useState<RenderJob | null>(null);
  const [view, setView] = useState<'document' | 'list'>('document');
  const [active, setActive] = useState(-1);
  const [sheet, setSheet] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [remember, setRemember] = useState(false);
  const [hasRemembered, setHasRemembered] = useState(false);
  const [acknowledged, setAcknowledged] = useState(false);
  // Android's share sheet does not say whether the file went out: the user confirms it (내보냈어요).
  const [exportUnconfirmed, setExportUnconfirmed] = useState(false);
  // Boxes 작성 완료 could not draw, with why: the sheet opens on the first one.
  const [problems, setProblems] = useState<Record<string, string>>({});
  const [draftSummary, setDraftSummary] = useState<DraftSummary | null>(null);
  const agentStatus = useQuery({ queryKey: ['agent-status', userId], queryFn: ({ signal }) => api.getAgentStatus(signal), enabled: !!userId, staleTime: 60_000 });
  const templateBytes = useRef<Uint8Array | null>(null);
  const filledBytes = useRef<Uint8Array | null>(null);
  // After export, the filled PDF stays in memory only for the 보관함에 저장 offer:
  // dropped (zeroed) once saved, on 다시 수정해서 만들기, or when the screen closes.
  const vaultBytes = useRef<Uint8Array | null>(null);
  const [vaultOffer, setVaultOffer] = useState<'none' | 'ready' | 'saved'>('none');
  const client = useQueryClient();
  const scroll = useRef<ScrollView>(null);
  const pageY = useRef<number[]>([]);
  const jobSeq = useRef(0);
  const pageWidth = Math.min(screenWidth - 32, 720);

  const fields: TemplateField[] = useMemo(() => template ? readingOrder(template.fields) : [], [template]);
  const prog = useMemo(() => progress(fields, state), [fields, state]);
  const problemIds = useMemo(() => new Set(Object.keys(problems)), [problems]);

  const jobKind = useRef<'template' | 'review'>('template');
  const render = useCallback((bytes: Uint8Array, kind: 'template' | 'review' = 'template') => { jobSeq.current += 1; jobKind.current = kind; setJob({ id: jobSeq.current, base64: bytesToBase64(bytes) }); }, []);

  const loadSeq = useRef(0);
  const load = useCallback(async () => {
    // Overlapping runs (account restored mid-load, 다시 시도) never overwrite a newer one.
    const seq = ++loadSeq.current;
    const live = () => seq === loadSeq.current;
    const show = (p: Phase) => { if (live()) setPhase(p); };
    show({ kind: 'loading', message: '공식 양식을 확인하고 있어요' }); setError('');
    let context: SuggestionContext = { profile: null, remembered: {}, today: new Date() };
    try {
      if (!programId) throw new Error('혜택 정보를 확인하지 못했어요. 혜택 화면에서 다시 열어 주세요.');
      const latest = await api.getProgram(programId);
      if (!live()) return;
      if (latest.program_type === 'financial_product') throw new Error('금융상품 신청서는 서류 작성 도우미를 지원하지 않아요. 기관 안내에 따라 작성해 주세요.');
      if (!resourceId) {
        // No form the app can fill: the copy sheet and the official page.
        setProgram(latest); setResource(null); setTemplate(null);
        const [profile, remembered] = await Promise.all([
          userId ? api.getProfile().then(r => r.profile).catch(() => null) : Promise.resolve(null),
          loadRemembered(userId),
        ]);
        if (!live()) return;
        context = { profile, remembered, today: new Date() };
        setCtx(context); setHasRemembered(Object.keys(remembered).length > 0);
        show({ kind: 'guided', reason: '앱에서 채울 수 있는 서식이 아직 없어요. 아래 내용을 공식 신청처의 양식에 옮겨 적어 주세요.', rows: guidedRows(context) });
        return;
      }
      const listed = latest.application_policy?.resources?.templates.find(r => r.id === resourceId);
      if (!listed) throw new Error('자료가 변경됐거나 확인 기한이 지났어요. 새 공고를 확인해 주세요.');
      const fresh = currentResource(latest, listed);
      setProgram(latest); setResource(fresh);
      const [profile, remembered] = await Promise.all([
        userId ? api.getProfile().then(r => r.profile).catch(() => null) : Promise.resolve(null),
        loadRemembered(userId),
      ]);
      if (!live()) return;
      context = { profile, remembered, today: new Date() };
      setCtx(context); setHasRemembered(Object.keys(remembered).length > 0);
      const guided = (reason: string) => show({ kind: 'guided', reason, rows: guidedRows(context) });
      const byFormat = chooseMode(fresh.file_format, null, null);
      const derivable = !!fresh.file_format && DERIVABLE_FORMATS.includes(fresh.file_format);
      if (byFormat.mode === 'guided' && fresh.file_format !== 'pdf' && !derivable) { guided(byFormat.reason); return; }
      if (Platform.OS === 'web') { guided('웹에서는 양식을 직접 작성할 수 없어요. 앱에서 이용하거나 아래 내용을 원본 양식에 옮겨 적어 주세요.'); return; }
      show({ kind: 'loading', message: '공식 양식을 내려받아 원본과 같은지 확인하고 있어요' });
      const serverRaw = api.getFormTemplate(programId, resourceId).catch(() => null);
      const parse = (raw: unknown) => { try { return raw ? parseTemplate(raw, fresh.content_hash ?? '', fresh.file_format) : null; } catch { return null; } };
      let serverTemplate: FormTemplate | null;
      let bytes: Uint8Array;
      if (fresh.file_format === 'pdf') {
        const [raw, official] = await Promise.all([serverRaw, downloadVerifiedTemplate(fresh)]);
        serverTemplate = parse(raw); bytes = official;
      } else {
        // HWP/HWPX: only the PDF Mazimi converted from this exact official file, and only with boxes for it.
        serverTemplate = parse(await serverRaw);
        const source = templateSource(fresh, serverTemplate, api.formPdfUrl(programId, resourceId));
        if (!live()) return;
        if (!source) { guided(byFormat.mode === 'guided' ? byFormat.reason : '아직 입력 칸 위치가 준비되지 않은 양식이에요.'); return; }
        bytes = await downloadVerifiedTemplate(source);
      }
      const analysis = await analyzePdf(bytes, serverTemplate?.derived ? serverTemplate.content_hash : fresh.content_hash ?? '');
      if (!live()) return;
      const mode = chooseMode('pdf', analysis, serverTemplate);
      if (mode.mode === 'guided') { guided(mode.reason); return; }
      templateBytes.current = bytes;
      setAspects(analysis.ok ? analysis.aspects : []);
      setTemplate(mode.template);
      // While this form is open, values said to the 비서 stay on this device.
      if (resourceId) setOpenForm({ programId, resourceId, template: mode.template });
      const base = buildDraft(mode.template.fields, context);
      // Values the user said to the 비서, handed over after an approved card
      // (lib/formFill/chatFill.ts): this account, this form, still fresh.
      const said = draft === '1' && resourceId && userId ? chatValuesFor(userId, programId, resourceId) : null;
      const built = said ? applyChatValues(base, mode.template.fields, said) : base;
      setState(built.state);
      setDraftSummary(draft === '1' ? built.summary : null);
      setPages(null); render(bytes);
      show({ kind: 'fill' });
    } catch (e) {
      if (e instanceof TemplateIntegrityError) { show({ kind: 'guided', reason: e.message, rows: guidedRows(context) }); return; }
      show({ kind: 'error', message: e instanceof Error ? e.message : '양식을 불러오지 못했어요.' });
    }
  }, [programId, resourceId, userId, render, draft]);

  useEffect(() => { void load(); }, [load]);
  // Leaving the form helper ends its form context.
  useEffect(() => () => { if (openFormContext()?.resourceId === resourceId) setOpenForm(null); }, [resourceId]);
  // router.push keeps this screen mounted: moving on to another screen drops the kept PDF too.
  useFocusEffect(useCallback(() => () => { if (vaultBytes.current) { dropVaultBytes(); setVaultOffer('none'); } }, []));
  // Leaving the screen drops every value from memory and removes cached files.
  useEffect(() => () => { templateBytes.current = null; dropFilledBytes(); dropVaultBytes(); void clearFormFillCache(); }, []);
  // 주민등록번호·계좌번호 may be on screen: no screenshots, recordings or app-switcher snapshots here.
  useEffect(() => {
    if (Platform.OS === 'web') return;
    void ScreenCapture.preventScreenCaptureAsync('form-fill').catch(() => {});
    if (Platform.OS === 'ios') void ScreenCapture.enableAppSwitcherProtectionAsync(0.6).catch(() => {});
    return () => {
      void ScreenCapture.allowScreenCaptureAsync('form-fill').catch(() => {});
      if (Platform.OS === 'ios') void ScreenCapture.disableAppSwitcherProtectionAsync().catch(() => {});
    };
  }, []);

  function onPages(id: number, rendered: RenderedPage[]) {
    if (id !== jobSeq.current) return;
    if (jobKind.current === 'review') setReviewPages(rendered); else setPages(rendered);
  }
  function onRenderError(id: number) {
    if (id !== -1 && id !== jobSeq.current) return;
    setRendererDown(true); setView('list');
  }

  async function openOriginal() {
    if (busy) return;
    if (!resource) {
      // No form file: the server-selected official page, re-read now.
      const popup = Platform.OS === 'web' ? window.open('about:blank', '_blank') : null;
      if (popup) popup.opener = null;
      setBusy(true); setError('');
      try {
        const url = resolveApplyUrl(await api.getProgram(programId));
        if (!url) throw new Error('공식 신청 주소를 확인하지 못했어요. 공고를 확인해 주세요.');
        if (popup) popup.location.replace(url); else await Linking.openURL(url);
      } catch (e) { popup?.close(); setError(e instanceof Error ? e.message : '공식 페이지를 열지 못했어요.'); }
      finally { setBusy(false); }
      return;
    }
    const popup = Platform.OS === 'web' ? window.open('about:blank', '_blank') : null;
    if (popup) popup.opener = null;
    setBusy(true); setError('');
    try {
      const fresh = currentResource(await api.getProgram(programId), resource);
      if (popup) popup.location.replace(fresh.url); else await Linking.openURL(fresh.url);
    } catch (e) { popup?.close(); setError(e instanceof Error ? e.message : '공식 양식을 열지 못했어요.'); }
    finally { setBusy(false); }
  }

  function focus(index: number) {
    if (index < 0 || index >= fields.length) return;
    setActive(index); setSheet(true);
    const f = fields[index];
    if (view === 'document' && pageY.current[f.page] !== undefined) {
      scroll.current?.scrollTo({ y: Math.max(0, pageY.current[f.page] + f.rect.y * (pageWidth / (aspects[f.page] || 0.707)) - 160), animated: true });
    }
  }
  // Footer "다음 항목": the next unfinished field, else simply the next one. The sheet's 이전/다음 step in order.
  const goNext = () => { const i = nextPending(fields, state, active, 1); focus(i >= 0 ? i : (active + 1) % fields.length); };
  const stepNext = () => focus((active + 1) % fields.length);
  const goPrev = () => focus(active <= 0 ? fields.length - 1 : active - 1);
  const activeField = active >= 0 ? fields[active] : null;
  const update = (next: FormState) => {
    setState(next); setAcknowledged(false);
    // A box the user changed is no longer the one that failed.
    setProblems(p => { const left = Object.fromEntries(Object.entries(p).filter(([id]) => next[id] === state[id])); return Object.keys(left).length === Object.keys(p).length ? p : left; });
  };

  async function finish() {
    if (!template || !templateBytes.current || !prog.canFinish || busy) return;
    setBusy(true); setError('');
    try {
      const out = await flattenForm(templateBytes.current, template, state, await loadFormFont());
      dropFilledBytes(); filledBytes.current = out;
      setReviewPages(null); setPhase({ kind: 'review' }); setAcknowledged(false); setExportUnconfirmed(false);
      if (!rendererDown) render(out, 'review');
    } catch (e) {
      if (e instanceof FieldDrawError) {
        // Take the user to the box: the sheet shows why, the box turns to the error tone.
        setProblems(Object.fromEntries(e.fieldIds.map(id => [id, e.message])));
        focus(fields.findIndex(f => f.id === e.fieldIds[0]));
      } else setError(e instanceof Error ? e.message : '작성본을 만들지 못했어요.');
    } finally { setBusy(false); }
  }

  /**
   * Opens the share sheet. Only a share the platform reports as done (iOS) or
   * the user confirms (Android, 내보냈어요) counts as an export: dismissing the
   * sheet leaves everything as it was, the values and the filled PDF included.
   */
  async function exportPdf() {
    if (!filledBytes.current || !acknowledged || busy || !program || !resource || !template) return;
    setBusy(true); setError('');
    try {
      const outcome = await shareFilledPdf(filledBytes.current, programFormName(program));
      if (outcome === 'shared') await completeExport();
      else if (outcome === 'unknown') setExportUnconfirmed(true);
    } catch (e) { setError(e instanceof Error ? e.message : '파일을 저장하지 못했어요.'); }
    finally { setBusy(false); }
  }
  /** 내보냈어요 (Android): the same export, behind the busy guard so a double tap or 수정하기 cannot interleave. */
  async function confirmExport() {
    if (busy) return;
    setBusy(true); setError('');
    try { await completeExport(); } finally { setBusy(false); }
  }
  async function completeExport() {
    if (!filledBytes.current || !program || !resource || !template) return;
    if (user) {
      if (remember) await saveRemembered(user.id, rememberable(fields, state)).catch(() => {});
      await addRecord(user.id, { program_id: program.id, program_title: program.title, resource_label: resource.label, template_hash: template.content_hash, filled_fields: progress(fields, state).completed }).catch(() => {});
    }
    // Sensitive values never outlive the export. The PDF itself is kept only to offer the encrypted vault.
    const cleared = sensitiveFilledLabels(fields, state);
    setState(s => clearSensitive(fields, s));
    dropVaultBytes(); setExportUnconfirmed(false);
    // Handed to the vault offer, the buffer moves (not zeroed); otherwise it is zeroed now.
    if (user && vaultFilesSupported()) { vaultBytes.current = filledBytes.current; filledBytes.current = null; setVaultOffer('ready'); } else { setVaultOffer('none'); dropFilledBytes(); }
    // The preview job holds the filled PDF as base64: drop it as well.
    setReviewPages(null); setJob(null);
    setPhase({ kind: 'done', cleared, records: (await loadRecords(user?.id)).filter(r => r.program_id === program.id).slice(0, 5) });
  }

  function dropVaultBytes() { vaultBytes.current?.fill(0); vaultBytes.current = null; }
  /** The filled PDF (주민등록번호·계좌번호) is zeroed before the reference goes, unless it moved to the vault offer. */
  function dropFilledBytes() { if (filledBytes.current && filledBytes.current !== vaultBytes.current) filledBytes.current.fill(0); filledBytes.current = null; }
  async function saveToVault() {
    const bytes = vaultBytes.current;
    if (!bytes || !user || !program || !resource || !template || busy) return;
    vaultBytes.current = null;
    setBusy(true); setError('');
    try {
      await saveFilledForm(user.id, bytes, { programId: program.id, programTitle: program.title, resourceId: resource.id, resourceLabel: resource.label, templateHash: template.content_hash });
      setVaultOffer('saved');
      void client.invalidateQueries({ queryKey: ['vault-documents', user.id] });
    } catch (e) {
      // saveFilledForm has zeroed the buffer unless a retry could help ('failed').
      if (e instanceof VaultSaveError && e.problem === 'failed') vaultBytes.current = bytes; else setVaultOffer('none');
      setError(e instanceof Error ? e.message : '보관함에 저장하지 못했어요.');
    } finally { setBusy(false); }
  }

  const title = '서류 작성 도우미';
  const renderer = <PdfRenderer job={job} width={pageWidth} onPages={onPages} onError={onRenderError} />;

  if (phase.kind === 'loading' || phase.kind === 'error') return <View style={ui.screen}>{renderer}<ScreenHeader title={title} back />
    <EmptyState loading={phase.kind === 'loading'} title={phase.kind === 'loading' ? phase.message : '양식을 준비하지 못했어요'} description={phase.kind === 'error' ? phase.message : '파일은 이 기기에서만 처리돼요.'} action={phase.kind === 'error' ? '다시 시도' : undefined} onAction={() => void load()} />
    {phase.kind === 'error' && resource && <View style={{ paddingHorizontal: 16 }}><Pressable accessibilityRole="button" onPress={() => void openOriginal()} style={ui.outlineButton}><Text style={ui.outlineButtonText}>원본 양식 열기</Text></Pressable></View>}
  </View>;

  if (phase.kind === 'guided') return <View style={ui.screen}>{renderer}<ScreenHeader title={title} back />
    <ScrollView contentContainerStyle={{ padding: 16, paddingBottom: 32 + insets.bottom }}>
      <Header program={program} resource={resource} derived={false} />
      <GuidedSheet reason={phase.reason} rows={phase.rows} busy={busy} onOpenOriginal={() => void openOriginal()} openLabel={resource ? undefined : '공식 신청 페이지 열기'} />
      {!!error && <Text accessibilityRole="alert" style={styles.error}>{error}</Text>}
    </ScrollView>
  </View>;

  if (phase.kind === 'done') return <View style={ui.screen}>{renderer}<ScreenHeader title={title} back />
    <ScrollView contentContainerStyle={{ padding: 16, gap: 12 }}>
      <Text style={ui.sectionTitle}>작성본을 내보냈어요</Text>
      <Text style={styles.body}>저장하거나 공유한 파일을 기관 안내에 따라 직접 제출해 주세요.</Text>
      {/* Says only what happened: the sensitive values this form actually had, and where the PDF is kept. */}
      {(!!phase.cleared.length || vaultOffer === 'ready') && <Text style={ui.muted}>{[
        phase.cleared.length ? `입력한 ${phase.cleared.join('·')}는 지웠어요.` : '',
        vaultOffer === 'ready' ? '작성본 파일은 보관함에 저장하거나 이 화면을 나가면 앱 메모리에서 지워져요.' : phase.cleared.length ? '다시 만들려면 새로 입력해야 해요.' : '',
      ].filter(Boolean).join(' ')}</Text>}
      {vaultOffer === 'ready' && <Pressable accessibilityRole="button" disabled={busy} onPress={() => void saveToVault()} style={ui.outlineButton}><Text style={ui.outlineButtonText}>{busy ? '저장 중…' : '보관함에 저장'}</Text></Pressable>}
      {vaultOffer === 'ready' && <Text style={ui.muted}>작성본은 이 기기에 암호화해 보관해요.</Text>}
      {vaultOffer === 'saved' && <Text accessibilityLiveRegion="polite" style={ui.muted}>보관함에 저장했어요. 서류 보관함의 기타에서 볼 수 있어요.</Text>}
      {!!error && <Text accessibilityRole="alert" style={styles.error}>{error}</Text>}
      <Pressable accessibilityRole="button" disabled={busy} onPress={() => { dropVaultBytes(); setVaultOffer('none'); setError(''); setPhase({ kind: 'fill' }); setAcknowledged(false); if (templateBytes.current && !pages && !rendererDown) render(templateBytes.current); }} style={ui.outlineButton}><Text style={ui.outlineButtonText}>다시 수정해서 만들기</Text></Pressable>
      <Text style={ui.sectionTitle}>다음 단계</Text>
      <Pressable accessibilityRole="button" onPress={() => router.push(`/apply-assistant?programId=${programId}`)} style={ui.primaryButton}><Text style={ui.primaryButtonText}>공식 신청 페이지로 가기</Text></Pressable>
      <Pressable accessibilityRole="button" onPress={() => router.push((agentStatus.data?.enabled ? `/agent?programId=${programId}&goal=applied` : `/application/${programId}`) as never)} style={ui.outlineButton}><Text style={ui.outlineButtonText}>신청을 마쳤어요 · 기록하기</Text></Pressable>
      {!!phase.records.length && <View style={styles.card}><Text style={styles.cardTitle}>이 기기의 작성 기록</Text>
        {phase.records.map(r => <Text key={r.id} style={ui.muted}>{new Date(r.created_at).toLocaleString('ko-KR')} · {applicationFormName(r.program_title)}</Text>)}</View>}
    </ScrollView>
  </View>;

  if (phase.kind === 'review') return <View style={ui.screen}>{renderer}<ScreenHeader title="작성본 확인" back />
    <ScrollView contentContainerStyle={{ padding: 16, paddingBottom: 24 }}>
      <Text style={styles.body}>기관에 내기 전에 모든 쪽을 확인해 주세요.</Text>
      {!rendererDown && (reviewPages ? reviewPages.map((p, i) => <Image key={i} accessibilityLabel={`작성본 ${i + 1}쪽`} source={{ uri: p.uri }} style={{ width: pageWidth, height: pageWidth * p.height / p.width, marginVertical: 6, borderWidth: 1, borderColor: '#F1F1F1' }} />)
        : <View style={{ paddingVertical: 32 }}><ActivityIndicator color="#34BB84" /></View>)}
      <View style={styles.card}>
        <Text style={styles.cardTitle}>입력한 내용</Text>
        {/* What the PDF holds: an unconfirmed suggestion is not drawn, so it reads as empty here too. */}
        {fields.map(f => { const s = state[f.id]; return <Text key={f.id} style={ui.muted}>{f.label}: {s?.status === 'suggested' ? '—' : summaryOf(f, s, '—')}</Text>; })}
      </View>
      <Pressable accessibilityRole="checkbox" accessibilityState={{ checked: acknowledged }} onPress={() => setAcknowledged(a => !a)} style={styles.ack}>
        <View style={[styles.check, acknowledged && styles.checked]}>{acknowledged ? <CheckMark color="#FFFFFF" /> : null}</View>
        <Text style={[styles.body, { flex: 1 }]}>모든 내용을 직접 확인했어요. 제출은 기관에 제가 직접 할게요.</Text>
      </Pressable>
      {!!error && <Text accessibilityRole="alert" style={styles.error}>{error}</Text>}
    </ScrollView>
    <View style={[styles.footer, { paddingBottom: Math.max(insets.bottom, 16) }]}>
      <Pressable accessibilityRole="button" disabled={busy} onPress={() => { dropFilledBytes(); setReviewPages(null); setExportUnconfirmed(false); setPhase({ kind: 'fill' }); if (!pages && templateBytes.current && !rendererDown) render(templateBytes.current); }} style={[ui.outlineButton, { flex: 1 }]}><Text style={ui.outlineButtonText}>수정하기</Text></Pressable>
      <Pressable accessibilityRole="button" disabled={!acknowledged || busy} onPress={() => void exportPdf()} style={[exportUnconfirmed ? ui.outlineButton : ui.primaryButton, { flex: exportUnconfirmed ? 1.4 : 2, paddingHorizontal: 8 }, (!acknowledged || busy) && { opacity: 0.4 }]}><Text style={exportUnconfirmed ? ui.outlineButtonText : ui.primaryButtonText}>{busy ? '준비 중…' : '파일로 저장·공유'}</Text></Pressable>
      {exportUnconfirmed && <Pressable accessibilityRole="button" disabled={busy} onPress={() => void confirmExport()} style={[ui.primaryButton, { flex: 1.4, paddingHorizontal: 8 }, busy && { opacity: 0.4 }]}><Text style={ui.primaryButtonText}>내보냈어요</Text></Pressable>}
    </View>
  </View>;

  // phase.kind === 'fill'
  const byPage = (page: number) => fields.map((field, index) => ({ field, index })).filter(x => x.field.page === page);
  return <View style={ui.screen}>{renderer}<ScreenHeader title={title} back />
    <View style={styles.status}>
      <View style={styles.progress}><View style={[styles.progressFill, { width: `${prog.total ? prog.completed / prog.total * 100 : 0}%` }]} /></View>
      <Text style={styles.statusText}>{prog.requiredRemaining ? `필수 항목 ${prog.requiredRemaining}개 남음` : '필수 항목 완료'}{prog.unconfirmed ? ` · 확인 필요 ${prog.unconfirmed}개` : ''}</Text>
      {/* Above the scroll view, so an error never ends up under the footer. */}
      {!!error && <Text accessibilityRole="alert" style={styles.fieldError}>{error}</Text>}
      {!rendererDown && <View style={styles.toggle}>{(['document', 'list'] as const).map(v => <Pressable key={v} accessibilityRole="tab" accessibilityState={{ selected: view === v }} onPress={() => setView(v)} style={[styles.toggleItem, view === v && styles.toggleOn]}><Text style={[styles.toggleText, view === v && { color: '#fff' }]}>{v === 'document' ? '문서' : '목록'}</Text></Pressable>)}</View>}
    </View>
    <ScrollView ref={scroll} contentContainerStyle={{ padding: 16, paddingBottom: 24 }}>
      <Header program={program} resource={resource} derived={!!template?.derived} />
      <Text style={styles.legal}>본인 작성 보조 · 입력 내용은 이 기기에서만 처리되고 서버로 보내지 않아요. 제출은 기관에 직접 해 주세요.</Text>
      {draftSummary && <Text accessibilityLiveRegion="polite" style={styles.draft}>{draftNote(draftSummary)}</Text>}
      {rendererDown && <Text style={ui.muted}>문서 미리보기를 불러오지 못해 목록으로 보여드려요. 작성본은 그대로 만들 수 있어요.</Text>}
      {view === 'document' && !rendererDown ? aspects.map((aspect, page) => <PageWithFields key={page} page={page} image={pages?.[page]} aspect={aspect} width={pageWidth} fields={byPage(page)} state={state} active={active} problems={problemIds} onPress={focus} onLayout={y => { pageY.current[page] = y; }} />)
        : fields.map((f, index) => { const s = state[f.id]; const group = f.group && f.group !== fields[index - 1]?.group ? template?.groups?.find(g => g.id === f.group) : undefined; return <React.Fragment key={f.id}>
          {group && <Text accessibilityRole="header" style={[styles.listLabel, { marginTop: 8 }]}>{groupHint(group)}</Text>}
          <Pressable accessibilityRole="button" onPress={() => focus(index)} style={styles.listRow}>
          <View style={{ flex: 1 }}><Text style={styles.listLabel}>{f.label}{f.required ? ' *' : ''} <Text style={ui.muted}>· {f.page + 1}쪽</Text></Text>
            <Text style={[ui.muted, s?.status === 'suggested' && { color: '#0095FF' }, !!problems[f.id] && { color: '#F75B2B' }]}>{s?.status === 'suggested' ? `확인 필요: ${summaryOf(f, s, '')}` : summaryOf(f, s, '비어 있음')}</Text></View>
          <FigmaIcon name="arrow" size={16} color="#E8E8E8" /></Pressable></React.Fragment>; })}
      {user && <View style={styles.card}>
        <View style={{ flexDirection: 'row', alignItems: 'center', gap: 12 }}>
          <Text style={[styles.body, { flex: 1 }]}>이름·연락처·주소·이메일을 이 기기에 암호화해 기억하기</Text>
          <FigmaToggle accessibilityLabel="연락처 정보 기억하기" value={remember} onValueChange={setRemember} />
        </View>
        <Text style={ui.muted}>켜면 다음 양식에서 제안값으로 보여드려요. 주민등록번호·계좌번호는 기억하지 않아요.</Text>
        {hasRemembered && <Pressable accessibilityRole="button" onPress={() => { void forgetRemembered(user.id).then(() => { setHasRemembered(false); setCtx(c => ({ ...c, remembered: {} })); }); }} style={{ minHeight: 36, justifyContent: 'center' }}><Text style={{ color: '#239969', fontSize: 13, lineHeight: 20 }}>기억한 정보 지우기</Text></Pressable>}
      </View>}
    </ScrollView>
    <View style={[styles.footer, { paddingBottom: Math.max(insets.bottom, 16) }]}>
      <Pressable accessibilityRole="button" onPress={goPrev} style={[ui.outlineButton, { flex: 1, paddingHorizontal: 8 }]}><Text style={ui.outlineButtonText}>이전</Text></Pressable>
      <Pressable accessibilityRole="button" onPress={goNext} style={[ui.outlineButton, { flex: 1, paddingHorizontal: 8 }]}><Text style={ui.outlineButtonText}>다음 항목</Text></Pressable>
      <Pressable accessibilityRole="button" disabled={!prog.canFinish || busy} onPress={() => void finish()} style={[ui.primaryButton, { flex: 1.4 }, (!prog.canFinish || busy) && { opacity: 0.4 }]}><Text style={ui.primaryButtonText}>{busy ? '만드는 중…' : '작성 완료'}</Text></Pressable>
    </View>
    {sheet && activeField && <FieldSheet key={activeField.id} field={activeField} state={state[activeField.id]} hint={hintFor(activeField, ctx)} position={`${active + 1} / ${fields.length}`}
      group={template?.groups?.find(g => g.id === activeField.group)} problem={problems[activeField.id]}
      onSave={(value, nav) => { const next = setValue(state, activeField, value, fields, template?.groups); update(next); if (nav === 'prev') goPrev(); else if (nav === 'next') stepNext(); else goNextAfter(next); }}
      onConfirm={() => { const next = confirmSuggestion(state, activeField, fields); update(next); goNextAfter(next); }}
      onNotApplicable={() => { const next = markNotApplicable(state, activeField); update(next); goNextAfter(next); }}
      onPrev={goPrev} onNext={stepNext} onClose={() => setSheet(false)} />}
  </View>;

  function goNextAfter(next: FormState) {
    const i = nextPending(fields, next, active, 1);
    if (i < 0 || i === active) setSheet(false); else focus(i);
  }
}

/** One line for a box's value in the list and the review summary. Sensitive numbers stay masked. */
function summaryOf(f: TemplateField, s: FieldState | undefined, empty: string): string {
  if (s?.status === 'na') return '해당 없음';
  const v = s && hasValue(s) ? s.value : null;
  if (!v) return empty;
  if (v.kind === 'signature') return '서명함';
  if (v.kind === 'check') return v.on ? '체크' : '체크 안 함';
  return f.type === 'stamp' ? `도장(${v.value})` : maskSensitive(f, v.value);
}

function Header({ program, resource, derived }: { program: ApiProgram | null; resource: ApplicationResource | null; derived: boolean }) {
  return <View style={{ marginBottom: 12 }}>
    <Text style={styles.eyebrow}>{program?.title}</Text>
    <Text style={styles.title}>{resource && program ? programFormName(program) : '공식 양식'}</Text>
    {resource && <Text style={ui.muted}>{derived
      ? `기관 공개 ${resource.file_format?.toUpperCase()} 원본을 마지미가 PDF로 옮긴 사본`
      : `${resource.file_format?.toUpperCase()} · 기관 공개 원본`}</Text>}
    {resource && derived && <Text style={ui.muted}>모양이 원본과 조금 다를 수 있어요. 기관이 원본 양식만 받는다면 원본 양식 열기로 확인해 주세요.</Text>}
  </View>;
}

const styles = StyleSheet.create({
  eyebrow: { fontSize: 12, lineHeight: 18, color: '#34BB84', marginBottom: 4 },
  title: { fontSize: 20, lineHeight: 32, fontWeight: '700', color: '#070428' },
  body: { fontSize: 13, lineHeight: 20, color: '#070428' },
  draft: { fontSize: 13, lineHeight: 20, color: '#070428', backgroundColor: '#0095FF14', borderRadius: 10, padding: 10, marginBottom: 12 },
  legal: { fontSize: 12, lineHeight: 18, color: '#239969', backgroundColor: '#EFFFF8', borderRadius: 10, padding: 10, marginBottom: 12 },
  error: { color: '#070428', marginTop: 12 },
  fieldError: { fontSize: 12, lineHeight: 18, color: '#F75B2B' },
  status: { paddingHorizontal: 16, paddingVertical: 10, borderBottomWidth: 1, borderColor: '#F1F1F1', gap: 6 },
  statusText: { fontSize: 13, lineHeight: 20, fontWeight: '600', color: '#070428' },
  progress: { height: 5, backgroundColor: '#F7F9FA', borderRadius: 4 },
  progressFill: { height: 5, backgroundColor: '#34BB84', borderRadius: 4 },
  toggle: { position: 'absolute', right: 16, top: 16, flexDirection: 'row', backgroundColor: '#F7F9FA', borderRadius: 14, padding: 2 },
  toggleItem: { paddingHorizontal: 12, minHeight: 28, justifyContent: 'center', borderRadius: 12 },
  toggleOn: { backgroundColor: '#191A54' },
  toggleText: { fontSize: 12, lineHeight: 18, color: '#5D5D62' },
  footer: { flexDirection: 'row', gap: 8, paddingHorizontal: 16, paddingTop: 12, borderTopWidth: 1, borderColor: '#F1F1F1', backgroundColor: '#fff' },
  listRow: { flexDirection: 'row', alignItems: 'center', minHeight: 56, paddingVertical: 10, borderBottomWidth: 1, borderBottomColor: '#F1F1F1' },
  listLabel: { fontSize: 14, lineHeight: 20, fontWeight: '600', color: '#070428' },
  card: { backgroundColor: '#F7F9FA', borderRadius: 16, padding: 14, marginTop: 16, gap: 6 },
  cardTitle: { fontSize: 14, lineHeight: 20, fontWeight: '700', color: '#070428' },
  ack: { flexDirection: 'row', alignItems: 'center', gap: 10, marginTop: 16, minHeight: 44 },
  check: { height: 22, width: 22, borderRadius: 6, borderWidth: 1, borderColor: '#F1F1F1', alignItems: 'center', justifyContent: 'center' },
  checked: { backgroundColor: '#34BB84', borderColor: '#34BB84' },
});
