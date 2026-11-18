import { StyleSheet } from 'react-native';
import { fig } from '@/constants/theme';

/** Shared readable layout for the assistant's discovery and comparison steps.
 * No frame of its own: Figma step titles 34002:3815, cards 34065:2930, filter pills 34065:3283. */
export const assistantStyles = StyleSheet.create({
  content: { padding: 16, paddingBottom: 48, gap: 16 },
  title: { ...fig.text.bold20, color: fig.color.black },
  heading: { ...fig.text.bold16, color: fig.color.black },
  body: { ...fig.text.body13, color: fig.color.gray66 },
  note: { ...fig.text.body12, color: fig.color.gray66 },
  card: { borderRadius: fig.component.card.radius, borderWidth: 1, borderColor: fig.color.grayF1, backgroundColor: fig.color.white, boxShadow: fig.shadow.card, padding: 16, gap: 12 },
  row: { borderTopWidth: 1, borderColor: fig.color.grayF7, paddingTop: 12, gap: 8 },
  chips: { flexDirection: 'row', flexWrap: 'wrap', gap: 8 },
  chip: { borderWidth: 1, borderColor: fig.color.secondary8, backgroundColor: fig.color.white, borderRadius: fig.component.filterPill.radius, paddingHorizontal: fig.component.filterPill.paddingHorizontal, paddingVertical: 12, minHeight: 44, justifyContent: 'center' },
  selected: { backgroundColor: fig.color.secondary8, borderColor: fig.color.secondary8 },
  notice: { ...fig.text.body13, color: fig.color.black },
});
