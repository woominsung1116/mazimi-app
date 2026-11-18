import React, { useState } from 'react';
import { Pressable, View, StyleSheet } from 'react-native';
import * as Clipboard from 'expo-clipboard';
import Text from '@/components/Text';
import { ui } from '@/components/FigmaUI';
import type { GuidedRow } from '@/lib/formFill/fields';

/**
 * Copy sheet for forms the app cannot fill (HWP, flat PDF without a box
 * definition, unsupported PDF): the usual fields with the user's values to
 * copy into the original form by hand.
 */
export default function GuidedSheet({ reason, rows, busy, onOpenOriginal, openLabel = '원본 양식 열기' }: { reason: string; rows: GuidedRow[]; busy: boolean; onOpenOriginal: () => void; openLabel?: string }) {
  const [copied, setCopied] = useState<string | null>(null);
  async function copy(row: GuidedRow) {
    if (!row.value || row.sensitive) return;
    await Clipboard.setStringAsync(row.value);
    setCopied(row.kind);
  }
  return <View style={{ gap: 12 }}>
    <View style={styles.notice}><Text style={styles.noticeText}>{reason}</Text></View>
    <Pressable accessibilityRole="button" disabled={busy} onPress={onOpenOriginal} style={[ui.primaryButton, busy && { opacity: 0.5 }]}><Text style={ui.primaryButtonText}>{busy ? '확인 중…' : openLabel}</Text></Pressable>
    <Text style={ui.sectionTitle}>옮겨 적을 내용</Text>
    {rows.map(row => <View key={row.kind} style={styles.row}>
      <View style={{ flex: 1 }}>
        <Text style={styles.label}>{row.label}</Text>
        {row.value ? <Text style={styles.value}>{row.value}</Text> : <Text style={ui.muted}>{row.hint ?? '직접 입력해 주세요.'}</Text>}
        {!!row.value && !!row.sourceLabel && <Text style={ui.muted}>{row.sourceLabel}</Text>}
      </View>
      {!!row.value && !row.sensitive && <Pressable accessibilityRole="button" accessibilityLabel={`${row.label} 복사`} onPress={() => void copy(row)} style={styles.copy}><Text style={styles.copyText}>{copied === row.kind ? '복사됨' : '복사'}</Text></Pressable>}
    </View>)}
  </View>;
}

const styles = StyleSheet.create({
  notice: { backgroundColor: '#F72B2B0A', borderRadius: 12, padding: 12 },
  noticeText: { fontSize: 13, lineHeight: 20, color: '#070428' },
  row: { flexDirection: 'row', alignItems: 'center', gap: 12, paddingVertical: 12, borderBottomWidth: 1, borderBottomColor: '#F1F1F1' },
  label: { fontSize: 13, lineHeight: 20, fontWeight: '600', color: '#070428' },
  value: { fontSize: 13, lineHeight: 20, color: '#070428', marginTop: 2 },
  copy: { minHeight: 36, paddingHorizontal: 14, borderRadius: 18, backgroundColor: '#EFFFF8', justifyContent: 'center' },
  copyText: { color: '#239969', fontSize: 13, lineHeight: 20, fontWeight: '600' },
});
