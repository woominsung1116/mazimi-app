import React, { useMemo, useRef, useState } from 'react';
import { PanResponder, Pressable, View, type LayoutChangeEvent } from 'react-native';
import Text from '@/components/Text';
import { fitSignature, MAX_SIGNATURE_POINTS, signatureSegments, type Signature, type SignatureFit, type Stroke } from '@/lib/formFill/signature';

/** Draws a signature's segments (box coordinates from `fit`) into width×height. */
export function Strokes({ signature, fit, width, height, color = '#0D0D1A', thickness = 2 }: { signature: Signature; fit: SignatureFit; width: number; height: number; color?: string; thickness?: number }) {
  const segments = signatureSegments(signature, fit).map(([x0, y0, x1, y1], i) => {
    const length = Math.max(thickness, Math.hypot(x1 - x0, y1 - y0));
    return <View key={i} style={{ position: 'absolute', left: (x0 + x1) / 2 - length / 2, top: (y0 + y1) / 2 - thickness / 2, width: length, height: thickness, borderRadius: thickness / 2, backgroundColor: color, transform: [{ rotate: `${Math.atan2(y1 - y0, x1 - x0)}rad` }] }} />;
  });
  return <View pointerEvents="none" style={{ width, height }}>{segments}</View>;
}

/** The signature as the PDF draws it: its ink fitted into the box (pdf.ts signaturePath uses the same fit). */
export function SignaturePreview({ signature, width, height, thickness }: { signature: Signature; width: number; height: number; thickness?: number }) {
  const fit = useMemo(() => fitSignature(signature, width, height), [signature, width, height]);
  return <Strokes signature={signature} fit={fit} width={width} height={height} thickness={thickness} />;
}

/** Finger-drawn signature. The strokes stay in memory; nothing is saved or uploaded. */
export function SignaturePad({ initial, onChange }: { initial: Signature | null; onChange: (value: Signature | null) => void }) {
  const [size, setSize] = useState({ width: 0, height: 0 });
  const [strokes, setStrokes] = useState<Stroke[]>(() => initial?.strokes ?? []);
  const live = useRef<Stroke[]>(strokes);
  const sizeRef = useRef(size);
  sizeRef.current = size;
  const commit = (next: Stroke[]) => { live.current = next; setStrokes(next); };
  const point = (x: number, y: number): [number, number] => [Math.min(1, Math.max(0, x / (sizeRef.current.width || 1))), Math.min(1, Math.max(0, y / (sizeRef.current.height || 1)))];
  const responder = useMemo(() => PanResponder.create({
    onStartShouldSetPanResponder: () => true,
    onMoveShouldSetPanResponder: () => true,
    onPanResponderTerminationRequest: () => false,
    onPanResponderGrant: e => {
      if (live.current.reduce((n, s) => n + s.length, 0) >= MAX_SIGNATURE_POINTS) return;
      commit([...live.current, [point(e.nativeEvent.locationX, e.nativeEvent.locationY)]]);
    },
    onPanResponderMove: e => {
      const all = live.current;
      if (!all.length || all.reduce((n, s) => n + s.length, 0) >= MAX_SIGNATURE_POINTS) return;
      const last = all[all.length - 1];
      const p = point(e.nativeEvent.locationX, e.nativeEvent.locationY);
      const prev = last[last.length - 1];
      if (Math.hypot((p[0] - prev[0]) * sizeRef.current.width, (p[1] - prev[1]) * sizeRef.current.height) < 2) return;
      commit([...all.slice(0, -1), [...last, p]]);
    },
    onPanResponderRelease: () => emit(),
    // A stroke cut off by another responder still counts.
    onPanResponderTerminate: () => emit(),
  }), [onChange]);
  // The pad's own shape is the signature's aspect (a 370×160 pad is 2.31, not a square).
  function emit() {
    const { width, height } = sizeRef.current;
    onChange(live.current.length && width && height ? { strokes: live.current, aspect: width / height } : null);
  }
  const onLayout = (e: LayoutChangeEvent) => setSize({ width: e.nativeEvent.layout.width, height: e.nativeEvent.layout.height });
  // On the pad the strokes stay where they were drawn: one pad unit is the pad's height.
  const padFit: SignatureFit = { scale: size.height, ox: 0, oy: 0 };
  return <View style={{ gap: 8 }}>
    <View onLayout={onLayout} {...responder.panHandlers} accessibilityLabel="서명 입력 영역. 손가락으로 서명해 주세요." style={{ height: 160, borderRadius: 12, borderWidth: 1, borderColor: '#F1F1F1', backgroundColor: '#F7F9FA', overflow: 'hidden' }}>
      {!strokes.length && <Text pointerEvents="none" style={{ position: 'absolute', alignSelf: 'center', top: 68, color: '#8894A1', fontSize: 13, lineHeight: 20 }}>여기에 손가락으로 서명해 주세요</Text>}
      {size.width > 0 && size.height > 0 && <Strokes signature={{ strokes, aspect: size.width / size.height }} fit={padFit} width={size.width} height={size.height} thickness={2.5} />}
    </View>
    <Pressable accessibilityRole="button" onPress={() => { commit([]); onChange(null); }} style={{ alignSelf: 'flex-end', minHeight: 36, justifyContent: 'center', paddingHorizontal: 8 }}><Text style={{ color: '#239969', fontSize: 13, lineHeight: 20 }}>다시 그리기</Text></Pressable>
  </View>;
}

/** Red seal preview, matching the PDF layout (one column ≤3 characters, 2×2 for 4). */
export function StampPreview({ name, size }: { name: string; size: number }) {
  const chars = [...name];
  const font = chars.length === 4 ? size * 0.3 : chars.length === 1 ? size * 0.5 : chars.length === 2 ? size * 0.38 : size * 0.27;
  return <View style={{ width: size, height: size, borderRadius: size / 2, borderWidth: Math.max(1, size * 0.04), borderColor: '#D11F1F', alignItems: 'center', justifyContent: 'center' }}>
    {chars.length === 4
      ? <View style={{ flexDirection: 'row-reverse' }}>{[0, 2].map(c => <View key={c}>{chars.slice(c, c + 2).map((ch, i) => <Text key={i} style={{ color: '#D11F1F', fontSize: font, lineHeight: font * 1.1, fontWeight: '700' }}>{ch}</Text>)}</View>)}</View>
      : chars.map((ch, i) => <Text key={i} style={{ color: '#D11F1F', fontSize: font, lineHeight: font * 1.05, fontWeight: '700' }}>{ch}</Text>)}
  </View>;
}
