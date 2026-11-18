import React from 'react';
import { Linking, StyleSheet, View } from 'react-native';
import Text from '@/components/Text';
import type { ApplicationProcedure, ApplicationStage } from '@/lib/api';
import { safeOfficialUrl } from '@/lib/assistantTypes';
import { fig } from '@/constants/theme';

const C = fig.color, T = fig.text;

/**
 * 신청 방법 body, Figma 상세 34065:11157: the organisation (13/600), then one
 * body text (12/18 Gray66) of "label : value" lines, then the notice's own
 * steps in the same text style. Only what the server derived from the
 * programme's fields is shown (services/application_procedure.rs).
 * `fallback` is the method text an older server sends without a procedure.
 */
export function ApplicationProcedureBody({ procedure, fallback }: { procedure?: ApplicationProcedure | null; fallback: string }) {
  const empty = !procedure || (!procedure.organization && !procedure.lines.length && !procedure.intro && !procedure.steps.length);
  if (empty) return <Text style={styles.body}>{fallback}</Text>;
  const numbered = procedure.ordered && procedure.steps.length > 1;
  return <View style={styles.block}>
    {procedure.organization ? <Text style={styles.organization}>{procedure.organization}</Text> : null}
    {procedure.lines.length ? <View>{procedure.lines.map((line, index) => {
      const url = line.url ? safeOfficialUrl(line.url) : null;
      const text = `${line.label}${line.value ? ` : ${line.value}` : ''}`;
      // One Text per line in the frame's body style; a linked line is its own focusable link.
      return url
        ? <Text key={index} accessibilityRole="link" accessibilityLabel={`${text} 열기`} onPress={() => { void Linking.openURL(url).catch(() => undefined); }} style={styles.body}>{text}</Text>
        : <Text key={index} style={styles.body}>{text}</Text>;
    })}</View> : null}
    {procedure.intro ? <Text style={styles.body}>{procedure.intro}</Text> : null}
    {procedure.steps.length === 1
      ? <Text style={styles.body}>{procedure.steps[0].title}{procedure.steps[0].detail ? `\n${procedure.steps[0].detail}` : ''}</Text>
      : procedure.steps.map((step, index) => <View key={index} style={styles.step}>
        <Text style={styles.body}>{numbered ? `${index + 1}.` : '·'}</Text>
        <Text style={[styles.body, styles.stepText]}>{step.title}{step.detail ? `\n${step.detail}` : ''}</Text>
      </View>)}
  </View>;
}

/** The stepper's accessible name of one stage. */
export function stageAccessibilityLabel(stage: ApplicationStage, index: number) {
  const state = stage.state === 'done' ? '완료' : stage.state === 'current' ? '현재 단계' : '예정';
  return `${index + 1}단계 ${stage.label}, ${state}`;
}

const styles = StyleSheet.create({
  block: { gap: 8 }, // 34065:11157
  organization: { ...T.semibold13, color: C.black }, // 34065:11158
  body: { ...T.body12, color: C.gray66 }, // 34065:11159
  step: { flexDirection: 'row', gap: 4 },
  stepText: { flex: 1 },
});
