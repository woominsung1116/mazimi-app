import React, { forwardRef, useEffect, useRef, useState } from "react";
import { ActivityIndicator, Pressable, StyleSheet, View, type GestureResponderEvent, type PressableProps, type PressableStateCallbackType, type StyleProp, type ViewStyle } from "react-native";
import { colors } from "@/constants/theme";

// 누를 수 있는 모든 것의 공통 버튼 (v1.0.0 피드백 F4.3).
// 누르는 동안 살짝 어두워지고, 동작이 도는 동안 스피너를 띄워 눌렸는지 알 수 있게 한다.
// react-native Pressable과 같은 props를 받으므로 import만 바꾸면 된다.

export type TappableProps = Omit<PressableProps, "onPress"> & {
  /** Promise를 돌려주면 끝날 때까지 스피너를 띄우고 다시 누르지 못하게 한다 */
  onPress?: (event: GestureResponderEvent) => unknown;
  /** 바깥에서 아는 진행 상태 (mutation.isPending, query.isFetching 등) */
  loading?: boolean;
  /** "darken": 누르는 동안 어두운 막을 덮는다 (기본). "opacity": 흐려진다 (어두운 배경의 버튼) */
  feedback?: "darken" | "opacity";
  spinnerColor?: string;
};

/** Pressed overlay colour; also read by scripts/check-home-feedback.cjs. */
export const PRESSED_OVERLAY = "#00000014";
export const PRESSED_OPACITY = 0.6;

const isPromise = (value: unknown): value is Promise<unknown> =>
  !!value && typeof (value as Promise<unknown>).then === "function";

const Tappable = forwardRef<View, TappableProps>(function Tappable(
  { onPress, loading = false, feedback = "darken", spinnerColor = colors.primary, disabled, style, children, accessibilityState, ...rest },
  ref,
) {
  const [running, setRunning] = useState(false);
  const mounted = useRef(true);
  useEffect(() => () => { mounted.current = false; }, []);
  const busy = loading || running;
  const press = (event: GestureResponderEvent) => {
    if (busy || !onPress) return;
    const result = onPress(event);
    if (!isPromise(result)) return;
    setRunning(true);
    result.catch(() => {}).finally(() => { if (mounted.current) setRunning(false); });
  };
  const resolveStyle = (state: PressableStateCallbackType): StyleProp<ViewStyle> => typeof style === "function" ? style(state) : style;
  return <Pressable
    ref={ref}
    {...rest}
    disabled={disabled || busy}
    accessibilityState={{ ...accessibilityState, disabled: !!(disabled ?? accessibilityState?.disabled), busy }}
    onPress={press}
    style={state => [resolveStyle(state), feedback === "opacity" && state.pressed && !busy && { opacity: PRESSED_OPACITY }]}
  >
    {state => {
      const radius = StyleSheet.flatten(resolveStyle(state))?.borderRadius;
      const corner = typeof radius === "number" ? { borderRadius: radius } : null;
      return <>
        {typeof children === "function" ? children(state) : children}
        {feedback === "darken" && state.pressed && !busy && <View pointerEvents="none" style={[StyleSheet.absoluteFill, corner, { backgroundColor: PRESSED_OVERLAY }]} />}
        {busy && <View pointerEvents="none" style={[StyleSheet.absoluteFill, corner, styles.busy]}><ActivityIndicator size="small" color={spinnerColor} accessibilityLabel="불러오는 중" /></View>}
      </>;
    }}
  </Pressable>;
});

export default Tappable;

const styles = StyleSheet.create({
  busy: { alignItems: "center", justifyContent: "center", backgroundColor: "#FFFFFFCC" },
});
