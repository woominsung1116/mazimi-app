import { Tabs } from "expo-router";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { FigmaIcon } from "@/components/FigmaUI";
import { fig } from "@/constants/theme";

export default function TabLayout() {
  const insets = useSafeAreaInsets();
  // Figma tab bar 34065:6364: five items, 52px over the home-indicator area, 24px icon box with
  // a 16px icon, 11px label; inactive items are Gray66 at 60%. 비서 has no tab in the frame: its
  // screen stays routable (/agent) and opens from the home banner carousel (lib/useAgentEntry).
  const bar = fig.component.tabBar;
  return <Tabs screenOptions={{ headerShown: false, tabBarActiveTintColor: fig.color.primary, tabBarInactiveTintColor: "#5D5D6299", tabBarStyle: { height: bar.height + Math.max(insets.bottom, 8), paddingTop: bar.paddingVertical, paddingBottom: Math.max(insets.bottom, 8), backgroundColor: bar.backgroundColor, borderTopWidth: 1, borderTopColor: bar.borderTopColor, borderTopLeftRadius: bar.radius, borderTopRightRadius: bar.radius, elevation: 0, shadowOpacity: 0 }, tabBarItemStyle: { height: bar.item }, tabBarLabelStyle: { fontFamily: "Pretendard", ...fig.text.body11 }, tabBarIconStyle: { width: bar.iconBox, height: bar.iconBox } }}>
    <Tabs.Screen name="index" options={{ title: "홈", tabBarIcon: ({ color }) => <FigmaIcon name="home" color={color} /> }} />
    <Tabs.Screen name="explore" options={{ title: "탐색", tabBarIcon: ({ color }) => <FigmaIcon name="search" color={color} /> }} />
    <Tabs.Screen name="agent" options={{ title: "비서", href: null }} />
    <Tabs.Screen name="manage" options={{ title: "관심 혜택", tabBarIcon: ({ color }) => <FigmaIcon name="bookmark" color={color} /> }} />
    <Tabs.Screen name="vault" options={{ title: "서류 보관함", tabBarIcon: ({ color }) => <FigmaIcon name="archive" color={color} /> }} />
    <Tabs.Screen name="profile" options={{ title: "마이페이지", tabBarIcon: ({ color }) => <FigmaIcon name="user" color={color} /> }} />
  </Tabs>;
}
