import React, { useEffect, useState } from 'react';
import { KeyboardAvoidingView, Modal, Platform, Pressable, ScrollView, TextInput, View, StyleSheet } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';
import Text from '@/components/Text';
import { ui } from '@/components/FigmaUI';
import { colors } from '@/constants/theme';
import { SignaturePad, StampPreview } from '@/components/formFill/Signature';
import { formatDate, isNumericSecret, isSensitive, isStampName, parseDate, seoulDate, textOf, textValue, type FieldGroup, type FieldState, type FieldValue, type Signature, type TemplateField } from '@/lib/formFill/fields';

export interface FieldSheetProps {
  field: TemplateField | null;
  state: FieldState | undefined;
  hint: string | null;
  position: string;
  /** The checkbox group this field is an option of, if any. */
  group?: FieldGroup;
  /** Why 작성 완료 could not draw this box (it opened the sheet here). */
  problem?: string;
  /** Saves the value (null clears it); `nav` moves to the previous/next field afterwards instead of the next pending one. */
  onSave: (value: FieldValue | null, nav?: 'prev' | 'next') => void;
  onConfirm: () => void;
  onNotApplicable: () => void;
  onPrev: () => void;
  onNext: () => void;
  onClose: () => void;
}

/** Heading shown above a grouped checkbox: the question and how many answers it takes. */
export function groupHint(group: FieldGroup): string {
  return group.mode === 'single' ? `${group.label} · 하나만 선택 (체크하면 같은 질문의 다른 선택지는 해제돼요)` : `${group.label} · 여러 개 선택 가능`;
}

/** One field at a time. A suggestion is shown with its source and must be confirmed or edited. */
export default function FieldSheet({ field, state, hint, position, group, problem, onSave, onConfirm, onNotApplicable, onPrev, onNext, onClose }: FieldSheetProps) {
  const insets = useSafeAreaInsets();
  const [draft, setDraft] = useState('');
  const [checked, setChecked] = useState<boolean | null>(null);
  const [signature, setSignature] = useState<Signature | null>(null);
  const [date, setDate] = useState({ y: '', m: '', d: '' });
  const [error, setError] = useState('');
  const [reveal, setReveal] = useState(false);
  const value = state?.status === 'na' ? null : state?.value ?? null;
  useEffect(() => {
    const text = textOf(state?.status === 'na' ? undefined : state);
    setDraft(text); setError('');
    setChecked(value?.kind === 'check' ? value.on : null);
    setSignature(value?.kind === 'signature' ? value : null);
    const parsed = field?.type === 'date' ? parseDate(text) : null;
    setDate(parsed ? { y: String(parsed.y), m: String(parsed.m), d: String(parsed.d) } : { y: '', m: '', d: '' });
  }, [field?.id, value, state?.status, field?.type]);
  if (!field) return null;
  const sensitive = isSensitive(field);
  const numericSecret = isNumericSecret(field);
  const suggested = state?.status === 'suggested';

  /** The value a save would store (null: empty), or an error message. */
  function pending(): { value: FieldValue | null } | { error: string } {
    if (!field) return { value: null };
    if (field.type === 'checkbox') return { value: checked === null ? null : { kind: 'check', on: checked } };
    if (field.type === 'signature') return { value: signature ? { kind: 'signature', ...signature } : null };
    if (field.type === 'date') {
      if (!date.y && !date.m && !date.d) return { value: null };
      try { return { value: textValue(formatDate({ y: Number(date.y), m: Number(date.m), d: Number(date.d) }, field.format)) }; }
      catch { return { error: '날짜를 다시 확인해 주세요.' }; }
    }
    if (field.type === 'stamp') {
      const name = draft.replace(/\s+/g, '');
      return name && !isStampName(name) ? { error: '도장 이름은 한글·영문 1~4자로 입력해 주세요.' } : { value: name ? textValue(name) : null };
    }
    return { value: draft ? textValue(draft) : null };
  }
  const next = pending();
  // Typed but not yet saved: navigation saves it instead of dropping it.
  const dirty = 'error' in next || !sameValue(next.value, value);
  function save(nav?: 'prev' | 'next') {
    const result = pending();
    if ('error' in result) { setError(result.error); return; }
    // An unanswered checkbox shows 체크 안 함 selected: saving it records that answer.
    onSave(field?.type === 'checkbox' && !result.value ? { kind: 'check', on: false } : result.value, nav);
  }
  const go = (nav: 'prev' | 'next') => { if (dirty) save(nav); else if (nav === 'prev') onPrev(); else onNext(); };
  const today = () => { const t = seoulDate(new Date()); setDate({ y: String(t.y), m: String(t.m), d: String(t.d) }); };

  return <Modal visible transparent animationType="slide" onRequestClose={onClose}>
    <Pressable accessibilityLabel="입력 닫기" style={styles.sheetBackdrop} onPress={onClose} />
    <KeyboardAvoidingView behavior={Platform.OS === 'ios' ? 'padding' : undefined}>
      <View style={[styles.sheet, { paddingBottom: Math.max(insets.bottom, 16), maxHeight: 620 }]}>
        <View style={styles.sheetGrip} />
        <ScrollView keyboardShouldPersistTaps="handled" scrollEnabled={field.type !== 'signature'}>
          <Text style={styles.position}>{position}{field.required ? ' · 필수' : ' · 선택'}</Text>
          <Text style={styles.sheetTitle}>{field.label}</Text>
          {suggested && !dirty && <View style={styles.suggestion}>
            <Text style={styles.suggestionText}>제안값 · {state?.sourceLabel ?? '마지미'}</Text>
            <Text style={styles.suggestionValue}>{field.type === 'stamp' ? `${textOf(state)} 도장` : textOf(state)}</Text>
            <Text style={ui.muted}>맞으면 확인을 눌러 주세요. 다르면 아래에서 고쳐 주세요.</Text>
            <Pressable accessibilityRole="button" onPress={onConfirm} style={[ui.primaryButton, { marginTop: 10 }]}><Text style={ui.primaryButtonText}>이 값이 맞아요</Text></Pressable>
          </View>}
          {!!hint && <Text style={[styles.hint, sensitive && { color: '#070428' }]}>{hint}</Text>}

          {(field.type === 'text' || field.type === 'multiline') && <TextInput
            accessibilityLabel={field.label} value={draft} onChangeText={setDraft} multiline={field.type === 'multiline'} secureTextEntry={numericSecret && !reveal && field.type === 'text'}
            placeholder={sensitive ? '작성할 때만 입력' : field.label} maxLength={500}
            keyboardType={numericSecret ? 'number-pad' : field.mapping === 'phone' ? 'phone-pad' : field.mapping === 'email' ? 'email-address' : 'default'}
            autoCorrect={false} autoCapitalize="none" autoComplete="off" textContentType="none" importantForAutofill="no"
            style={[styles.input, field.type === 'multiline' && { minHeight: 110, textAlignVertical: 'top' }]} />}
          {numericSecret && field.type === 'text' && <Pressable accessibilityRole="button" onPress={() => setReveal(r => !r)} style={{ alignSelf: 'flex-end', minHeight: 36, justifyContent: 'center', paddingHorizontal: 8 }}><Text style={styles.chipText}>{reveal ? '숨기기' : '입력한 번호 보기'}</Text></Pressable>}

          {field.type === 'date' && <View style={{ gap: 8 }}>
            <View style={{ flexDirection: 'row', gap: 8, alignItems: 'center' }}>
              <TextInput accessibilityLabel="연도" value={date.y} onChangeText={y => setDate(d => ({ ...d, y: y.replace(/\D/g, '').slice(0, 4) }))} keyboardType="number-pad" placeholder="YYYY" style={[styles.input, { flex: 2 }]} />
              <Text>년</Text>
              <TextInput accessibilityLabel="월" value={date.m} onChangeText={m => setDate(d => ({ ...d, m: m.replace(/\D/g, '').slice(0, 2) }))} keyboardType="number-pad" placeholder="MM" style={[styles.input, { flex: 1 }]} />
              <Text>월</Text>
              <TextInput accessibilityLabel="일" value={date.d} onChangeText={v => setDate(d => ({ ...d, d: v.replace(/\D/g, '').slice(0, 2) }))} keyboardType="number-pad" placeholder="DD" style={[styles.input, { flex: 1 }]} />
              <Text>일</Text>
            </View>
            <View style={{ flexDirection: 'row', justifyContent: 'space-between', alignItems: 'center' }}>
              <Text style={ui.muted}>양식 표기: {field.format ?? 'YYYY.MM.DD'}</Text>
              <Pressable accessibilityRole="button" onPress={today} style={styles.chip}><Text style={styles.chipText}>오늘</Text></Pressable>
            </View>
          </View>}

          {field.type === 'checkbox' && group && <Text accessibilityRole="header" style={ui.muted}>{groupHint(group)}</Text>}
          {field.type === 'checkbox' && <View style={{ flexDirection: 'row', gap: 8 }}>
            {([[true, '체크함'], [false, '체크 안 함']] as const).map(([on, label]) => <Pressable key={label} accessibilityRole={group?.mode === 'multi' ? 'checkbox' : 'radio'} accessibilityState={{ checked: (checked ?? false) === on }} onPress={() => setChecked(on)} style={[ui.outlineButton, { flex: 1 }, (checked ?? false) === on && styles.selected]}><Text style={ui.outlineButtonText}>{label}</Text></Pressable>)}
          </View>}

          {field.type === 'signature' && <SignaturePad initial={signature} onChange={setSignature} />}

          {field.type === 'stamp' && <View style={{ gap: 10, alignItems: 'center' }}>
            <TextInput accessibilityLabel="도장에 넣을 이름" value={draft} onChangeText={setDraft} maxLength={4} placeholder="이름 (1~4자)" autoCorrect={false} style={[styles.input, { alignSelf: 'stretch' }]} />
            {isStampName(draft.replace(/\s+/g, '')) && <StampPreview name={draft.replace(/\s+/g, '')} size={84} />}
            <Text style={ui.muted}>간단한 이름 도장 이미지예요. 인감 등 등록된 도장이 필요한 서류는 실물 도장을 사용해 주세요.</Text>
          </View>}

          {!!(error || (problem && !dirty)) && <Text accessibilityRole="alert" style={styles.error}>{error || problem}</Text>}
          <Pressable accessibilityRole="button" onPress={() => save()} style={[ui.primaryButton, { marginTop: 16 }]}><Text style={ui.primaryButtonText}>{suggested ? '고친 값으로 저장' : '저장'}</Text></Pressable>
          <View style={styles.row}>
            <Pressable accessibilityRole="button" onPress={() => go('prev')} style={[ui.outlineButton, styles.flex]}><Text style={ui.outlineButtonText}>이전 항목</Text></Pressable>
            <Pressable accessibilityRole="button" onPress={onNotApplicable} style={[ui.outlineButton, styles.flex]}><Text style={ui.outlineButtonText}>해당 없음</Text></Pressable>
            <Pressable accessibilityRole="button" onPress={() => go('next')} style={[ui.outlineButton, styles.flex]}><Text style={ui.outlineButtonText}>다음 항목</Text></Pressable>
          </View>
        </ScrollView>
      </View>
    </KeyboardAvoidingView>
  </Modal>;
}

const styles = StyleSheet.create({
  // Bottom-sheet chrome lives here since the shared OptionSheet styles left FigmaUI.
  sheetBackdrop: { flex: 1, backgroundColor: '#11111199' },
  sheet: { backgroundColor: '#fff', borderTopLeftRadius: 20, borderTopRightRadius: 20, paddingHorizontal: 16 },
  sheetGrip: { width: 40, height: 4, borderRadius: 2, backgroundColor: '#F7F9FA', alignSelf: 'center', marginTop: 10, marginBottom: 16 },
  sheetTitle: { fontSize: 16, lineHeight: 24, fontWeight: '700', color: colors.textPrimary, marginBottom: 8 },
  position: { fontSize: 12, lineHeight: 18, color: '#34BB84', marginBottom: 4 },
  suggestion: { backgroundColor: '#0095FF14', borderRadius: 12, padding: 12, marginBottom: 12 },
  suggestionText: { fontSize: 12, lineHeight: 18, color: '#0095FF', fontWeight: '600' },
  suggestionValue: { fontSize: 16, lineHeight: 24, fontWeight: '700', color: '#070428', marginVertical: 6 },
  hint: { fontSize: 12, lineHeight: 18, color: '#5D5D62', marginBottom: 10 },
  input: { minHeight: 48, borderWidth: 1, borderColor: '#F1F1F1', borderRadius: 10, paddingHorizontal: 12, fontSize: 16, lineHeight: 24, color: '#070428' },
  chip: { paddingHorizontal: 14, minHeight: 36, borderRadius: 18, backgroundColor: '#EFFFF8', justifyContent: 'center' },
  chipText: { color: '#239969', fontSize: 13, lineHeight: 20, fontWeight: '600' },
  selected: { borderColor: '#34BB84', backgroundColor: '#EFFFF8' },
  row: { flexDirection: 'row', gap: 8, marginTop: 10 },
  flex: { flex: 1, paddingHorizontal: 4 },
  error: { fontSize: 12, lineHeight: 18, color: '#F75B2B', marginTop: 8 },
});

function sameValue(a: FieldValue | null, b: FieldValue | null): boolean {
  if (!a || !b) return a === b;
  if (a.kind === 'text' && b.kind === 'text') return a.value === b.value;
  if (a.kind === 'check' && b.kind === 'check') return a.on === b.on;
  // A signature is the same only as the very strokes it was loaded from.
  return a.kind === 'signature' && b.kind === 'signature' && a.strokes === b.strokes;
}
