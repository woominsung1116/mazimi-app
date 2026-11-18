import { Alert, Platform } from "react-native";

// react-native-web의 Alert.alert는 아무것도 띄우지 않는다(빈 함수).
// 웹에서도 보여야 하는 안내·확인은 이 두 함수로 띄운다. lib/navigation.ts의 window.confirm 패턴과 같다.

/** 확인 버튼 하나짜리 안내. 웹은 window.alert, 앱은 Alert.alert. */
export function notify(title: string, message?: string): void {
  if (Platform.OS === "web") {
    window.alert(message ? `${title}\n${message}` : title);
    return;
  }
  Alert.alert(title, message);
}

/** 취소/확인 두 갈래 확인. 웹은 window.confirm, 앱은 Alert.alert. */
export function confirmAction(title: string, message: string, confirmText: string, onConfirm: () => void, { destructive = false }: { destructive?: boolean } = {}): void {
  if (Platform.OS === "web") {
    if (window.confirm(`${title}\n${message}`)) onConfirm();
    return;
  }
  Alert.alert(title, message, [
    { text: "취소", style: "cancel" },
    { text: confirmText, style: destructive ? "destructive" : "default", onPress: onConfirm },
  ]);
}
