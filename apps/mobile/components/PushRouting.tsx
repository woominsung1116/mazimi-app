import { useEffect } from "react";
import { Platform } from "react-native";
import { useRouter } from "expo-router";
import * as Notifications from "expo-notifications";
import { setupNotificationHandlers, syncPushIfGranted } from "@/lib/notifications";
import { useAuthStore } from "@/store/auth";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** 푸시 알림을 누르면 해당 혜택 상세로 보내고, 로그인한 기기는 알림 받을 곳으로 다시 등록한다 */
export default function PushRouting() {
  const router = useRouter();
  const userId = useAuthStore(s => s.user?.id);
  useEffect(() => { if (userId) void syncPushIfGranted(); }, [userId]);
  useEffect(() => {
    if (Platform.OS === "web") return;
    // 같은 알림이 리스너와 마지막 응답 양쪽으로 들어와도 한 번만 연다
    const handled = new Set<string>();
    const open = (response: Notifications.NotificationResponse) => {
      const key = response.notification.request.identifier;
      if (handled.has(key)) return;
      handled.add(key);
      void Notifications.clearLastNotificationResponseAsync().catch(() => {});
      const id = response.notification.request.content.data?.programId;
      if (typeof id === "string" && UUID.test(id)) router.push(`/programs/${id}`);
    };
    // 앱이 꺼진 상태에서 알림을 눌러 켠 경우
    void Notifications.getLastNotificationResponseAsync().then(response => { if (response) open(response); }).catch(() => {});
    return setupNotificationHandlers(open);
  }, []);
  return null;
}
