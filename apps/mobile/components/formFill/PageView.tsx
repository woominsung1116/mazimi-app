import React from 'react';
import { ActivityIndicator, Image, Pressable, View, StyleSheet } from 'react-native';
import Text from '@/components/Text';
import { SignaturePreview, StampPreview } from '@/components/formFill/Signature';
import type { RenderedPage } from '@/components/formFill/PdfRenderer';
import { hitSlopFor } from '@/lib/formFill/touch';
import { hasValue, isNumericSecret, type FormState, type TemplateField } from '@/lib/formFill/fields';

const COLORS = {
  required: { border: '#F75B2B', fill: '#F72B2B0A' },
  optional: { border: '#8894A1', fill: '#8894A13B' },
  suggested: { border: '#0095FF', fill: '#0095FF14' },
  confirmed: { border: '#34BB84', fill: '#2A8F6614' },
  na: { border: '#E1E1E1', fill: '#8894A13B' },
};

/** On-screen only: 주민등록번호·계좌번호 show their first 6 characters, the rest masked. 예금주 is a name and shows as typed. */
export function maskSensitive(field: TemplateField, value: string): string {
  return isNumericSecret(field) ? value.slice(0, 6) + '•'.repeat(Math.max(0, value.length - 6)) : value;
}

/** One page image with tappable input boxes on top of it (Modusign-style in-place filling). */
export function PageWithFields({ page, image, aspect, width, fields, state, active, problems, onPress, onLayout }: {
  page: number; image: RenderedPage | undefined; aspect: number; width: number;
  fields: { field: TemplateField; index: number }[]; state: FormState; active: number;
  /** Fields 작성 완료 could not draw: shown in the required (error) tone until changed. */
  problems?: ReadonlySet<string>;
  onPress?: (index: number) => void; onLayout?: (y: number) => void;
}) {
  const height = width / aspect;
  const frames = fields.map(({ field }) => ({ left: field.rect.x * width, top: field.rect.y * height, w: field.rect.w * width, h: field.rect.h * height }));
  return <View onLayout={e => onLayout?.(e.nativeEvent.layout.y)} style={{ marginBottom: 12 }}>
    <Text style={styles.pageLabel}>{page + 1}쪽</Text>
    <View style={[styles.page, { width, height }]}>
      {image ? <Image accessibilityIgnoresInvertColors source={{ uri: image.uri }} style={{ width, height }} resizeMode="stretch" />
        : <View style={[styles.placeholder, { width, height }]}><ActivityIndicator color="#34BB84" /></View>}
      {fields.map(({ field, index }, i) => {
        const s = state[field.id];
        const tone = problems?.has(field.id) ? COLORS.required : s?.status === 'suggested' ? COLORS.suggested : s?.status === 'confirmed' && s.value ? COLORS.confirmed : s?.status === 'na' ? COLORS.na : field.required ? COLORS.required : COLORS.optional;
        const w = field.rect.w * width, h = field.rect.h * height;
        const value = s && s.status !== 'na' && hasValue(s) ? s.value : null;
        const filled = !!value;
        return <Pressable key={field.id} disabled={!onPress} onPress={() => onPress?.(index)} hitSlop={hitSlopFor(frames[i], frames)}
          accessibilityRole="button" accessibilityLabel={`${field.label}${field.required ? ' 필수' : ''} ${s?.status === 'suggested' ? '확인 필요' : filled ? '입력됨' : s?.status === 'na' ? '해당 없음' : '비어 있음'}`}
          style={[styles.box, { left: field.rect.x * width, top: field.rect.y * height, width: w, height: h, borderColor: tone.border, backgroundColor: onPress ? tone.fill : 'transparent', borderWidth: onPress ? (index === active ? 2 : 1) : 0 }]}>
          {value?.kind === 'check' ? (value.on ? <Text style={{ fontSize: Math.min(h, w) * 0.8, lineHeight: Math.min(h, w) * 0.9, textAlign: 'center' }}>✓</Text> : null)
            : value?.kind === 'signature' ? <SignaturePreview signature={value} width={w} height={h} thickness={1.2} />
            : value?.kind === 'text' && field.type === 'stamp' ? <View style={{ alignItems: 'center', justifyContent: 'center', flex: 1 }}><StampPreview name={value.value} size={Math.min(w, h) * 0.95} /></View>
            // One line shrinks to the box like the PDF does (layoutText) instead of being cut with "…".
            : value?.kind === 'text' ? <Text numberOfLines={field.type === 'multiline' ? 4 : 1} adjustsFontSizeToFit minimumFontScale={0.4} style={{ fontSize: Math.max(6, Math.min(h * 0.62, 12)), lineHeight: Math.max(6, Math.min(h * 0.62, 12)) * 1.25, color: s?.status === 'suggested' ? '#0095FF' : '#0D0D1A', paddingHorizontal: 2 }}>{maskSensitive(field, value.value)}</Text>
            : null}
        </Pressable>;
      })}
    </View>
  </View>;
}

const styles = StyleSheet.create({
  pageLabel: { fontSize: 11, lineHeight: 16, color: '#8894A1', marginBottom: 4 },
  page: { backgroundColor: '#fff', borderWidth: 1, borderColor: '#F1F1F1', overflow: 'hidden' },
  placeholder: { alignItems: 'center', justifyContent: 'center', backgroundColor: '#F7F9FA' },
  box: { position: 'absolute', borderRadius: 3, justifyContent: 'center', overflow: 'hidden' },
});
