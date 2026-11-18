// Guest and onboarding rules from the v1.0.0 device feedback, checked against
// the shipped modules with in-memory storage. No network, no real accounts.
//   - a guest leaves nothing on the device and cannot save conditions
//   - personalised recommendations ask for login only (no device-only path)
//   - the nickname is required before the first onboarding step moves on
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");
const { create } = require("zustand");

const root = path.resolve(__dirname, "..");
const read = relative => fs.readFileSync(path.join(root, relative), "utf8");

/** Loads `relative` with `mocks` for its imports; local modules listed in `files` load for real. */
function loader(mocks, files = {}) {
  const loaded = new Map();
  function load(relative) {
    const filename = path.join(root, relative);
    if (loaded.has(filename)) return loaded.get(filename).exports;
    const module = { exports: {} }; loaded.set(filename, module);
    const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
    new Function("require", "module", "exports", source)(name => {
      if (files[name]) return load(files[name]);
      if (name in mocks) return mocks[name];
      throw new Error(`Unmocked dependency ${name} in ${relative}`);
    }, module, module.exports);
    return module.exports;
  }
  return load;
}

// The real design tokens (constants/theme.ts has no runtime imports).
const theme = loader({ "react-native": {} })("constants/theme.ts");

/** AsyncStorage that records every call. */
function recordingStorage() {
  const calls = [], memory = new Map([["mazimi-onboarding-v1", JSON.stringify({ state: { nickname: "익명1", region: "busan", birthYear: 2001 }, version: 0 })]]);
  const storage = {
    getItem: async key => { calls.push(["get", key]); return memory.get(key) ?? null; },
    setItem: async (key, value) => { calls.push(["set", key]); memory.set(key, value); },
    removeItem: async key => { calls.push(["remove", key]); memory.delete(key); },
  };
  return { storage, calls, memory };
}

function loadConditions({ user = null, stored = {} } = {}) {
  const { storage, calls } = recordingStorage();
  const saves = [];
  const api = { getProfile: async () => ({ profile: stored }), saveProfile: async body => { saves.push(body); return { profile: body }; } };
  const load = loader({
    zustand: { create },
    "@react-native-async-storage/async-storage": { default: storage, __esModule: true },
    "./api": { api, ApiRequestError: class extends Error {} },
    "@/store/auth": { useAuthStore: { getState: () => ({ user }) } },
  }, { "@/constants/regions": "constants/regions.ts", "./districts.generated": "constants/districts.generated.ts", "@/store/onboarding": "store/onboarding.ts", "./profileCodes": "lib/profileCodes.ts", "@/lib/profileCodes": "lib/profileCodes.ts", "./birthDate": "lib/birthDate.ts", "./writeEffects": "lib/writeEffects.ts", "./optionalAnswers": "lib/optionalAnswers.ts", "@/lib/optionalAnswers": "lib/optionalAnswers.ts" });
  return { conditions: load("lib/conditions.ts"), store: load("store/onboarding.ts").useOnboardingStore, saves, calls };
}

const draft = () => ({ nickname: "마지미", region: "busan", cityCode: "26410", birthYear: new Date().getFullYear() - 24, enrollmentStatus: "enrolled", schoolName: "", employmentStatus: "", incomeBracket: null });
const client = { setQueryData() {}, invalidateQueries: async () => {} };

test("the onboarding draft lives in memory only: typing conditions writes nothing to the device", async () => {
  const { store, calls } = loadConditions();
  const s = store.getState();
  s.setNickname("마지미"); s.setRegion("busan"); s.setCityCode("26410"); s.setEnrollmentStatus("enrolled"); s.setIncomeBracket(3);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(calls, [], "no AsyncStorage read or write");
  assert.doesNotMatch(read("store/onboarding.ts"), /zustand\/middleware|AsyncStorage|persist\(/);
});

test("a guest cannot save conditions: nothing is sent and the draft is untouched", async () => {
  const { conditions, store, saves, calls } = loadConditions({ user: null });
  assert.equal(await conditions.saveConditions(client, undefined, draft()), false);
  assert.equal(saves.length, 0);
  assert.equal(store.getState().region, "", "a refused guest save does not fill the draft");
  assert.deepEqual(calls, []);
});

test("a member's save goes to the server profile", async () => {
  const { conditions, saves } = loadConditions({ user: { id: "member-a" } });
  assert.equal(await conditions.saveConditions(client, "member-a", draft()), true);
  assert.equal(saves.length, 1);
  assert.equal(saves[0].region_code, "busan");
  assert.equal(saves[0].nickname, "마지미");
});

test("재학·휴학 leaves 취업 상태 empty: untouched saves null, a pick is saved, 선택 안 함 stays null", async () => {
  const { conditions, store, saves } = loadConditions({ user: { id: "member-a" } });
  const s = store.getState();
  s.setNickname("마지미"); s.setRegion("busan"); s.setAge("24");
  // 학적 step, then 건너뛰기 on 소득·취업 (the same save() as the button).
  store.getState().chooseEnrollment("enrolled", "");
  assert.equal(store.getState().employmentStatus, "", "no answer");
  assert.equal("employmentSuggestion" in store.getState(), false, "nothing is pre-shown: the field reads 선택 안 함");
  await conditions.saveConditions(client, "member-a", { ...store.getState() });
  assert.equal(saves.at(-1).employment_status, null, "unknown stays unknown");
  store.getState().setEmploymentStatus("unemployed");
  await conditions.saveConditions(client, "member-a", { ...store.getState() });
  assert.equal(saves.at(-1).employment_status, "unemployed", "the pick is the answer");
  // 선택 안 함, then the 과정 follow-up toggled on and off and another 학적: still no answer.
  store.getState().setEmploymentStatus("");
  for (const [status, schoolType] of [["enrolled", "graduate"], ["enrolled", ""], ["on_leave", ""]]) {
    store.getState().chooseEnrollment(status, schoolType);
    assert.equal(store.getState().employmentStatus, "", `${status}/${schoolType || "-"}: 선택 안 함 stays`);
    assert.equal("employmentSuggestion" in store.getState(), false);
  }
  await conditions.saveConditions(client, "member-a", { ...store.getState() });
  assert.equal(saves.at(-1).employment_status, null, "선택 안 함 saves null");
  store.getState().setEmploymentStatus("employed");
  store.getState().chooseEnrollment("enrolled", "university");
  assert.equal(store.getState().employmentStatus, "employed", "a 학적 change keeps the answer");
});

test("내 정보 수정: a 학적 pick leaves 취업 상태 as answered; only a pick is saved", async () => {
  const { conditions, saves } = loadConditions({ user: { id: "member-a" } });
  const codes = loader({}, {})("lib/profileCodes.ts");
  assert.equal(codes.suggestedEmployment, undefined, "no suggestion rule is left");
  const { pickEnrollment } = codes;
  let edit = pickEnrollment({ ...draft(), enrollmentStatus: "graduated", schoolName: "부산대학교" }, "on_leave");
  assert.equal(edit.enrollmentStatus, "on_leave");
  assert.equal(edit.employmentStatus, "", "the field reads 선택 안 함");
  assert.equal(edit.schoolName, "부산대학교");
  await conditions.saveConditions(client, "member-a", edit);
  assert.equal(saves.at(-1).employment_status, null);
  edit = { ...edit, employmentStatus: "unemployed" };
  await conditions.saveConditions(client, "member-a", edit);
  assert.equal(saves.at(-1).employment_status, "unemployed", "the pick is the answer");
  edit = pickEnrollment({ ...edit, employmentStatus: "" }, "on_leave");
  assert.equal(edit.employmentStatus, "", "선택 안 함 stays after the 학적 follow-up");
  await conditions.saveConditions(client, "member-a", edit);
  assert.equal(saves.at(-1).employment_status, null);
  assert.equal(pickEnrollment(edit, "").schoolName, "", "no 학적, no school");
  // Queued updates in one batch compose (functional updaters on one draft).
  const queued = [current => pickEnrollment(current, "on_leave"), current => ({ ...current, schoolName: "부산대학교" }), current => ({ ...current, employmentStatus: "employed" })];
  const end = queued.reduce((state, update) => update(state), { ...draft(), enrollmentStatus: "graduated" });
  assert.deepEqual([end.enrollmentStatus, end.schoolName, end.employmentStatus], ["on_leave", "부산대학교", "employed"]);
});

test("a profile save sends no identity or server bookkeeping: the server uses the session", async () => {
  // `added_later`: a column the server adds after this build. POST /profile nulls what a body omits,
  // so a stored column this build does not know must go back unchanged; only identity and bookkeeping are dropped.
  const stored = { user_id: "member-a", profile_version: 4, updated_at: "2026-11-01T00:00:00Z", profile_image_url: null, added_later: "x", school_year: 2, kosaf_support_bracket: 5 };
  const { conditions, saves } = loadConditions({ user: { id: "member-a" }, stored });
  await conditions.saveConditions(client, "member-a", draft());
  for (const key of ["user_id", "profile_version", "updated_at", "profile_image_url"]) assert.equal(key in saves.at(-1), false, key);
  assert.equal(saves.at(-1).added_later, "x", "an unknown stored column is echoed back unchanged");
  assert.equal(saves.at(-1).kosaf_support_bracket, 5, "stored answers still come back");
});

test("start-up removes the guest draft an older build left on the device", async () => {
  const { storage, calls, memory } = recordingStorage();
  const load = loader({
    "@react-native-async-storage/async-storage": { default: storage, __esModule: true },
    "expo-router": { useRouter: () => ({}) },
    "@/store/auth": { useAuthStore: () => null },
    "./navigation": { requireLogin() {} },
  });
  const guest = load("lib/guest.ts");
  await guest.purgeLegacyGuestData();
  assert.equal(memory.has("mazimi-onboarding-v1"), false);
  assert.deepEqual(calls, [["remove", "mazimi-onboarding-v1"]]);
  // A storage failure never blocks start-up.
  storage.removeItem = async () => { throw new Error("disk"); };
  await guest.purgeLegacyGuestData();
  assert.match(read("app/_layout.tsx"), /purgeLegacyGuestData\(\)/);
});

test("personalised recommendations ask a guest to log in through the shared dialog; there is no device-only path", () => {
  assert.doesNotMatch(read("lib/navigation.ts"), /requireLoginOrConditions/, "the confirm/Alert path is gone");
  const copy = loader({ "./deadline": { deadlineLabel: () => "" }, "./api": {}, "./displayText": {}, "./verifiedSections": {} })("lib/displayCopy.ts");
  for (const [file, reason] of [["app/programs/[id].tsx", "GUEST_CONDITIONS_REASON"], ["app/(tabs)/explore.tsx", "GUEST_LOCAL_REASON"], ["components/RecommendationList.tsx", "GUEST_RECOMMEND_REASON"]]) {
    const source = read(file);
    assert.match(source, new RegExp(`login\\.prompt\\(${reason}\\)`), file);
    assert.match(source, /\{login\.dialog\}/, `${file} renders LoginPromptDialog`);
    assert.doesNotMatch(copy[reason], /이 기기/, reason);
  }
});

test("browsing without login opens the tabs, never the condition onboarding", () => {
  const login = read("app/login.tsx");
  const guestButton = login.slice(login.indexOf("로그인 없이 둘러보기") - 400, login.indexOf("로그인 없이 둘러보기"));
  assert.match(guestButton, /router\.replace\("\/\(tabs\)"\)/);
  assert.doesNotMatch(guestButton, /\/onboarding/);
  // Condition screens send a guest who arrives by link to login.
  assert.match(read("components/OnboardingFrame.tsx"), /if \(isGuest\) router\.replace\("\/login"\)/);
  assert.match(read("app/profile-edit.tsx"), /if \(guest\) router\.replace\("\/login"\)/);
});

test("no screen invents an anonymous name such as 익명1", () => {
  const dirs = ["app", "components", "lib", "store"];
  const offenders = [];
  const walk = dir => { for (const entry of fs.readdirSync(path.join(root, dir), { withFileTypes: true })) { const rel = path.join(dir, entry.name); if (entry.isDirectory()) walk(rel); else if (/\.tsx?$/.test(entry.name) && /익명\s*\d|익명 ?사용자/.test(read(rel))) offenders.push(rel); } };
  dirs.forEach(walk);
  assert.deepEqual(offenders, []);
  assert.match(read("app/(tabs)/profile.tsx"), /안녕하세요, 마지미예요/);
});

function loadLockedSection({ isGuest, os = "ios" }) {
  const opened = [];
  const element = (type, props) => ({ type, props });
  const load = loader({
    react: { default: {}, __esModule: true },
    "react/jsx-runtime": { jsx: element, jsxs: element, Fragment: "Fragment" },
    "react-native": { Platform: { OS: os }, Pressable: "Pressable", View: "View", StyleSheet: { create: x => x, absoluteFill: {} } },
    "./FigmaUI": { FigmaIcon: "FigmaIcon" },
    "./Text": { default: "Text", __esModule: true },
    "@/constants/theme": theme,
    "@/lib/guest": { useGuest: () => ({ isGuest, restoring: false, openLogin: () => opened.push("/login?return=1"), promptLogin() {} }) },
  });
  return { LockedSection: load("components/LockedSection.tsx").default, opened };
}
const flatten = node => !node || typeof node !== "object" ? [node] : [node, ...[].concat(node.props?.children ?? []).flatMap(child => typeof child === "function" ? flatten(child({ pressed: false })) : flatten(child))];

test("LockedSection: members see the content as-is; guests get a veil whose tap opens login", () => {
  const content = { type: "View", props: { children: "추천 목록" } };
  const member = loadLockedSection({ isGuest: false });
  assert.deepEqual(member.LockedSection({ children: content }).props.children, content);
  const guest = loadLockedSection({ isGuest: true });
  const tree = guest.LockedSection({ children: content, message: "로그인하면 볼 수 있어요" });
  const nodes = flatten(tree);
  const preview = nodes.find(n => n?.props?.pointerEvents === "none");
  assert.ok(preview, "the preview cannot be pressed");
  assert.equal(preview.props.importantForAccessibility, "no-hide-descendants");
  const veil = nodes.find(n => n?.type === "Pressable");
  assert.equal(veil.props.accessibilityLabel, "로그인 필요. 로그인하면 볼 수 있어요");
  assert.ok(nodes.some(n => n === "로그인 필요"));
  veil.props.onPress();
  assert.deepEqual(guest.opened, ["/login?return=1"]);
  // `locked` overrides the guest state either way.
  assert.deepEqual(guest.LockedSection({ children: content, locked: false }).props.children, content);
});

test("nickname is required: blank or whitespace-only never passes", () => {
  const load = loader({ "./api": { api: {}, ApiRequestError: class extends Error {} }, "@/store/auth": { useAuthStore: { getState: () => ({}) } }, zustand: { create } }, { "@/constants/regions": "constants/regions.ts", "./districts.generated": "constants/districts.generated.ts", "@/store/onboarding": "store/onboarding.ts", "./profileCodes": "lib/profileCodes.ts", "@/lib/profileCodes": "lib/profileCodes.ts", "./birthDate": "lib/birthDate.ts", "./writeEffects": "lib/writeEffects.ts", "./optionalAnswers": "lib/optionalAnswers.ts", "@/lib/optionalAnswers": "lib/optionalAnswers.ts" });
  const { validNickname, NICKNAME_MAX } = load("lib/conditions.ts");
  assert.equal(validNickname(""), false);
  assert.equal(validNickname("   "), false);
  assert.equal(validNickname("마지미"), true);
  assert.equal(validNickname(" 마지미 "), true);
  assert.equal(validNickname("가".repeat(NICKNAME_MAX)), true);
  assert.equal(validNickname("가".repeat(NICKNAME_MAX + 1)), false);
});

function renderNicknameStep(nickname) {
  const element = (type, props) => ({ type, props });
  const pushes = [];
  const load = loader({
    react: { default: {}, __esModule: true, useEffect() {}, useRef: () => ({ current: null }), useState: initial => [initial, () => {}] },
    "react/jsx-runtime": { jsx: element, jsxs: element, Fragment: "Fragment" },
    "react-native": { Pressable: "Pressable", TextInput: "TextInput" },
    "expo-router": { useRouter: () => ({ push: path => pushes.push(path) }), useLocalSearchParams: () => ({}) },
    "@tanstack/react-query": { useQuery: () => ({ data: undefined, isError: false, isLoading: false }) },
    "@/components/Text": { default: "Text", __esModule: true },
    "@/components/OnboardingFrame": { default: "OnboardingFrame", onboardingStyle: {}, __esModule: true },
    "@/store/onboarding": { useOnboardingStore: Object.assign(() => ({ nickname, setNickname() {} }), { setState() {} }) },
    "@/store/auth": { useAuthStore: () => ({ id: "member-a" }) },
    "@/lib/api": { api: {}, ApiRequestError: class extends Error {} },
    "@/lib/conditions": { NICKNAME_MAX: 30, validNickname: value => value.trim().length > 0 && value.trim().length <= 30 },
    "@/lib/optionalAnswers": { savedOptionalAnswers: () => ({}) },
  });
  const Onboarding = load("app/onboarding/index.tsx").default;
  return { frame: Onboarding(), pushes };
}

test("the nickname step blocks 다음 while empty and explains why", () => {
  for (const blank of ["", "  "]) {
    const { frame, pushes } = renderNicknameStep(blank);
    assert.equal(frame.type, "OnboardingFrame");
    assert.equal(frame.props.disabled, true, `blocked for ${JSON.stringify(blank)}`);
    assert.equal(typeof frame.props.onBlocked, "function", "a tap on the blocked button shows the inline error");
    assert.deepEqual(pushes, []);
  }
  const { frame } = renderNicknameStep("마지미");
  assert.equal(frame.props.disabled, false);
  frame.props.next();
  const input = [].concat(frame.props.children).find(child => child?.type === "TextInput");
  assert.equal(input.props.accessibilityLabel, "닉네임 (필수)");
  assert.doesNotMatch(input.props.placeholder, /선택/);
});

test("OnboardingFrame: a disabled 다음 calls onBlocked, never next", () => {
  const element = (type, props) => ({ type, props });
  const load = loader({
    react: { default: {}, __esModule: true, useEffect() {} },
    "react/jsx-runtime": { jsx: element, jsxs: element, Fragment: "Fragment" },
    "react-native": { ActivityIndicator: "ActivityIndicator", KeyboardAvoidingView: "KAV", Platform: { OS: "ios" }, Pressable: "Pressable", ScrollView: "ScrollView", StyleSheet: { create: x => x }, View: "View" },
    "expo-router": { useRouter: () => ({ replace() {}, canGoBack: () => true, back() {} }) },
    "react-native-safe-area-context": { useSafeAreaInsets: () => ({ top: 0, bottom: 0 }) },
    "./Text": { default: "Text", __esModule: true },
    "./FigmaUI": { FigmaIcon: "FigmaIcon", ui: {} },
    "@/constants/theme": theme,
    "@/lib/guest": { useGuest: () => ({ isGuest: false }) },
    "./SessionProblem": { SessionErrorState: "SessionErrorState", useSessionProblem: () => sessionProblem },
  });
  let sessionProblem = false;
  const Frame = load("components/OnboardingFrame.tsx").default;
  const calls = [];
  const tree = Frame({ title: "t", description: "d", children: null, next: () => calls.push("next"), disabled: true, onBlocked: () => calls.push("blocked") });
  const button = (function find(node) { if (!node || typeof node !== "object") return null; if (node.type === "Pressable" && node.props.accessibilityRole === "button" && node.props.accessibilityState) return node; for (const child of [].concat(node.props?.children ?? [])) { const hit = find(child); if (hit) return hit; } return null; })(tree);
  assert.equal(button.props.disabled, false, "still tappable so the reason can be shown");
  button.props.onPress();
  assert.deepEqual(calls, ["blocked"]);
  const plain = Frame({ title: "t", description: "d", children: null, next() {}, disabled: true });
  const plainButton = (function find(node) { if (!node || typeof node !== "object") return null; if (node.type === "Pressable" && node.props.accessibilityState) return node; for (const child of [].concat(node.props?.children ?? [])) { const hit = find(child); if (hit) return hit; } return null; })(plain);
  assert.equal(plainButton.props.disabled, true);
  // A member whose session could not be restored sees the retry state, not the form.
  sessionProblem = true;
  const outage = Frame({ title: "t", description: "d", children: "form", next() {} });
  assert.equal(outage.props.children.type, "SessionErrorState");
});

test("date of birth: 만 나이 on the KST date, and the server's bounds", () => {
  const { manAge, birthDateProblem, kstToday, parseBirthDate } = loader({})("lib/birthDate.ts");
  const today = { year: 2026, month: 10, day: 23 };
  assert.equal(manAge({ year: 2002, month: 10, day: 23 }, today), 24, "birthday today counts");
  assert.equal(manAge({ year: 2002, month: 10, day: 24 }, today), 23, "birthday tomorrow does not");
  assert.equal(birthDateProblem("2002-02-30", today), "invalid");
  assert.equal(birthDateProblem("2026-10-24", today), "future");
  assert.equal(birthDateProblem("1919-12-31", today), "too_old");
  assert.equal(birthDateProblem("2012-10-24", today), "too_young", "만 13세");
  assert.equal(birthDateProblem("2012-10-23", today), null, "만 14세 on the day");
  assert.equal(parseBirthDate("2002-5-1"), null, "only YYYY-MM-DD");
  // 2026-10-22 15:30 UTC is already 2026-10-23 in Korea.
  assert.deepEqual(kstToday(new Date(Date.UTC(2026, 9, 22, 15, 30))), today);
});

test("a save with a date of birth sends birth_date and the matching birth_year; without one it never resends a stored date", async () => {
  const { conditions, saves } = loadConditions({ user: { id: "member-a" } });
  assert.equal(await conditions.saveConditions(client, "member-a", { ...draft(), birthYear: 1999, birthDate: "2001-03-15" }), true);
  assert.equal(saves.at(-1).birth_date, "2001-03-15");
  assert.equal(saves.at(-1).birth_year, 2001, "birth_year follows birth_date (a mismatch is a 400)");
  assert.equal(await conditions.saveConditions(client, "member-a", { ...draft(), birthDate: "2001-02-30" }), false, "an impossible date is refused");
  await conditions.saveConditions(client, "member-a", draft());
  assert.equal(saves.at(-1).birth_date, undefined);
});

test("region picker: the hint names only the levels a sido has; 세종 skips the second step", () => {
  const regions = loader({}, { "./districts.generated": "constants/districts.generated.ts" })("constants/regions.ts");
  assert.equal(regions.districtLevelHint("seoul"), "구 선택");
  assert.equal(regions.districtLevelHint("busan"), "구·군 선택", "기장군");
  assert.equal(regions.districtLevelHint("gyeonggi"), "시·군 선택", "수원시 영통구 counts as a 시");
  assert.equal(regions.districtLevelHint("jeju"), "시 선택");
  assert.equal(regions.districtLevelHint("sejong"), "", "nothing to pick: no hint, no second step");
  const element = (type, props) => ({ type, props });
  const field = loader({
    react: { default: {}, __esModule: true, useState: v => [v, () => {}] },
    "react/jsx-runtime": { jsx: element, jsxs: element, Fragment: "Fragment" },
    "react-native": { Modal: "Modal", Pressable: "Pressable", ScrollView: "ScrollView", StyleSheet: { create: x => x, absoluteFillObject: {} }, TextInput: "TextInput", useWindowDimensions: () => ({ height: 800 }), View: "View" },
    "@expo/vector-icons": { Ionicons: "Ionicons" },
    "react-native-safe-area-context": { useSafeAreaInsets: () => ({ bottom: 0 }) },
    "./Text": { default: "Text", __esModule: true },
    "./FigmaUI": { FigmaIcon: "FigmaIcon" },
    "@/constants/theme": theme,
  }, { "@/constants/regions": "constants/regions.ts", "./districts.generated": "constants/districts.generated.ts" })("components/RegionField.tsx");
  assert.equal(field.regionDisplay("busan", "26350"), "부산광역시 해운대구", "the field shows the final choice in full");
  assert.equal(field.regionDisplay("busan", ""), "부산광역시");
  assert.equal(field.regionDisplay("busan", "41117"), "부산광역시", "another sido's district is ignored");
  assert.equal(field.regionDisplay("gyeonggi", "41192"), "경기도 부천시 원미구");
  assert.equal(field.regionDisplay("sejong", "36110"), "세종특별자치시");
  assert.equal(field.regionDisplay("", ""), "");
  const source = read("components/RegionField.tsx");
  assert.match(source, /if \(!hasDistrictChoice\(code\)\) \{ onChange\(code, ""\); close\(\); return; \}/);
  assert.equal((source.match(/accessibilityRole="radiogroup"/g) ?? []).length, 2);
});

test("enrollment: each status has its own follow-up and the values stay the server's codes", async () => {
  const element = (type, props) => ({ type, props });
  const images = Object.fromEntries(["enrolled", "leave", "graduated", "none"].map(n => [`../assets/figma/student-${n}.png`, 1]));
  const picker = loader({
    ...images,
    react: { default: {}, __esModule: true },
    "react/jsx-runtime": { jsx: element, jsxs: element, Fragment: "Fragment" },
    "react-native": { Image: "Image", Pressable: "Pressable", StyleSheet: { create: x => x }, View: "View" },
    "./Text": { default: "Text", __esModule: true },
    "./OnboardingFrame": { onboardingStyle: {} },
    "@/constants/theme": theme,
  }, { "@/lib/profileCodes": "lib/profileCodes.ts" })("components/EnrollmentPicker.tsx");
  const { ENROLLMENT_CHOICES, chooseEnrollment, enrollmentCard } = picker;
  const codes = new Set(["enrolled", "on_leave", "graduated", "research_student", ""]);
  for (const choice of ENROLLMENT_CHOICES) {
    assert.ok(codes.has(choice.value), choice.title);
    assert.ok(!choice.followUps || (choice.followUps.length >= 1 && choice.followUps.length <= 2), `${choice.title}: 1-2 follow-ups`);
    for (const f of choice.followUps ?? []) {
      if (f.patch.status) assert.ok(codes.has(f.patch.status));
      if (f.patch.schoolType) assert.ok(["university", "graduate"].includes(f.patch.schoolType), "mazimi_core::canonical_school_type");
    }
  }
  const none = ENROLLMENT_CHOICES.find(c => c.value === "");
  assert.doesNotMatch(none.description, /학적을 완료/, "해당 없음 no longer reads like 졸업");
  const research = ENROLLMENT_CHOICES.filter(c => (c.followUps ?? []).some(f => f.patch.status === "research_student")).map(c => c.value);
  assert.deepEqual(research, ["graduated"], "수료 후 연구생 only under 졸업");
  assert.equal(enrollmentCard("research_student"), "graduated");
  assert.deepEqual(chooseEnrollment("graduated", { status: "enrolled", schoolType: "graduate" }), { status: "graduated", schoolType: "graduate" });
  assert.deepEqual(chooseEnrollment("graduated", { status: "research_student", schoolType: "" }), { status: "research_student", schoolType: "" });
  assert.deepEqual(chooseEnrollment("on_leave", { status: "enrolled", schoolType: "university" }), { status: "on_leave", schoolType: "university" });
  assert.deepEqual(chooseEnrollment("", { status: "enrolled", schoolType: "graduate" }), { status: "", schoolType: "" }, "해당 없음 clears the course");

  const { conditions, saves } = loadConditions({ user: { id: "member-a" } });
  await conditions.saveConditions(client, "member-a", { ...draft(), schoolType: "graduate" });
  assert.equal(saves.at(-1).school_type, "graduate");
  assert.equal(saves.at(-1).enrollment_status, "enrolled");
  await conditions.saveConditions(client, "member-a", { ...draft(), enrollmentStatus: "", schoolType: "" });
  assert.equal(saves.at(-1).school_type, null);
  assert.equal(saves.at(-1).enrollment_status, null);
});

test("the 학자금 지원구간 field links to KOSAF's bracket page with the login note", () => {
  const source = read("components/ConditionFields.tsx");
  assert.match(source, /KOSAF_BRACKET_URL = "https:\/\/www\.kosaf\.go\.kr\/ko\/tuition\.do\?pg=tuition04_09_01&type=tuition"/);
  assert.match(source, /<Text style=\{styles\.kosafText\}>내 지원구간 확인하기<\/Text><FigmaIcon name="link"/, "the external link is the Figma link icon, not a ↗ glyph");
  assert.match(source, /※ 한국장학재단 로그인 필요 \(신규 산정 시 약 8주 소요\)/);
  assert.match(source, /<KosafBracketLink \/>/, "rendered with every IncomeBracketSelect (onboarding step 3, 내 정보 수정)");
});

test("an API outage with a stored session is not a guest: no locks, no login redirect (H1)", () => {
  let state;
  const load = loader({
    "@react-native-async-storage/async-storage": { default: {}, __esModule: true },
    "expo-router": { useRouter: () => ({ push() {} }) },
    "@/store/auth": { useAuthStore: selector => selector(state) },
    "./navigation": { requireLogin() {} },
  });
  const guest = load("lib/guest.ts");
  const cases = [
    [{ isLoading: true, user: null, sessionError: false }, false, "restoring"],
    [{ isLoading: false, user: { id: "m" }, sessionError: false }, false, "member"],
    [{ isLoading: false, user: null, sessionError: true }, false, "stored session, API unreachable"],
    [{ isLoading: true, user: null, sessionError: true }, false, "retrying after an outage"],
    [{ isLoading: false, user: null, sessionError: false }, true, "real guest"],
  ];
  for (const [s, expected, label] of cases) {
    state = s;
    assert.equal(guest.useIsGuest(), expected, `useIsGuest: ${label}`);
    assert.equal(guest.useGuest().isGuest, expected, `useGuest: ${label}`);
  }
  // Condition screens show the retry state instead of bouncing a member to /login.
  const frame = read("components/OnboardingFrame.tsx");
  assert.match(frame, /if \(sessionProblem\) return .*<SessionErrorState \/>/);
  const edit = read("app/profile-edit.tsx");
  assert.match(edit, /const guest = useIsGuest\(\)/);
  assert.match(edit, /if \(sessionProblem\) return .*<SessionErrorState \/>/);
  assert.doesNotMatch(edit, /!restoring && !user/, "profile-edit uses the shared guest rule");
});

test("profile edit: a stored birth year alone never blocks saving; the date is required only once edited (M5)", async () => {
  const { birthDateSave, BIRTH_DATE_HINTS } = loader({})("lib/birthDate.ts");
  const today = { year: 2026, month: 10, day: 23 };
  // Only birth_year stored, field untouched: saves with the stored year, soft hint, no red error.
  assert.deepEqual(birthDateSave({ birthDate: "", storedYear: 2001, touched: false }, today), { ok: true, birthDate: "", birthYear: 2001, showError: false, hint: "stored_year" });
  // Edited but unfinished: now required, red error.
  assert.deepEqual(birthDateSave({ birthDate: "", storedYear: 2001, touched: true }, today), { ok: false, birthDate: "", birthYear: 0, showError: true, hint: null });
  // A full date wins.
  assert.deepEqual(birthDateSave({ birthDate: "2001-03-04", storedYear: 2001, touched: true }, today), { ok: true, birthDate: "2001-03-04", birthYear: 2001, showError: false, hint: null });
  // Nothing stored: blocked, but only a grey hint until the field is edited.
  assert.deepEqual(birthDateSave({ birthDate: "", storedYear: 0, touched: false }, today), { ok: false, birthDate: "", birthYear: 0, showError: false, hint: "missing" });
  // A stored year under the minimum age is not usable.
  assert.equal(birthDateSave({ birthDate: "", storedYear: 2020, touched: false }, today).ok, false);
  assert.ok(BIRTH_DATE_HINTS.stored_year && BIRTH_DATE_HINTS.missing);
  // The nickname-only save sends the stored year and no birth_date.
  const { conditions, saves } = loadConditions({ user: { id: "member-a" } });
  const kept = birthDateSave({ birthDate: "", storedYear: 2001, touched: false });
  assert.equal(await conditions.saveConditions(client, "member-a", { ...draft(), nickname: "새닉네임", birthYear: kept.birthYear, birthDate: kept.birthDate }), true);
  assert.equal(saves[0].nickname, "새닉네임");
  assert.equal(saves[0].birth_year, 2001);
  assert.equal(saves[0].birth_date, undefined);
  const edit = read("app/profile-edit.tsx");
  assert.match(edit, /const canSave = dob\.ok &&/);
  assert.match(edit, /showError=\{dob\.showError\}/);
  assert.doesNotMatch(edit, /showError=\{!ageValid\}/);
});

test("validAge counts on the KST year: 2026-12-31 15:30 UTC is already 2027 in Seoul", () => {
  const { conditions } = loadConditions();
  const { kstToday } = loader({})("lib/birthDate.ts");
  const newYearInSeoul = kstToday(new Date("2026-12-31T15:30:00Z"));
  assert.equal(newYearInSeoul.year, 2027);
  assert.equal(conditions.validAge(2012, newYearInSeoul), true, "15 in 2027 (KST)");
  assert.equal(conditions.validAge(2012, { year: 2026, month: 12, day: 31 }), false, "14 by the local 2026 year");
  assert.doesNotMatch(read("lib/conditions.ts"), /new Date\(\)\.getFullYear\(\)/);
});

test("members never see a login prompt: restoring and unconfirmed sessions are 'pending', only a guest is nudged (item 2)", () => {
  let state;
  const load = loader({
    "@react-native-async-storage/async-storage": { default: {}, __esModule: true },
    "expo-router": { useRouter: () => ({ push() {} }) },
    "@/store/auth": { useAuthStore: selector => selector(state) },
    "./navigation": { requireLogin() {} },
  });
  const { memberStateOf } = load("lib/guest.ts");
  assert.equal(memberStateOf({ isLoading: true, user: null, sessionError: false }), "pending", "app start with a stored session");
  assert.equal(memberStateOf({ isLoading: false, user: null, sessionError: true }), "pending-error");
  assert.equal(memberStateOf({ isLoading: true, user: null, sessionError: true }), "pending", "retrying");
  assert.equal(memberStateOf({ isLoading: false, user: { id: "m" }, sessionError: false }), "member");
  assert.equal(memberStateOf({ isLoading: false, user: null, sessionError: false }), "guest");
  // No members-only screen decides "guest" from `!user` alone.
  const screens = ["app/(tabs)/agent.tsx", "app/assistant.tsx", "app/assistant-search.tsx", "app/benefit-assistant.tsx", "app/apply-assistant.tsx", "app/compare.tsx", "app/agent-history.tsx", "app/(tabs)/manage.tsx", "app/(tabs)/profile.tsx", "app/notifications.tsx", "app/applications.tsx", "app/account.tsx"];
  for (const file of screens) {
    const source = read(file);
    assert.match(source, /<NonMemberState/, `${file} uses NonMemberState`);
    assert.doesNotMatch(source, /action="로그인"/, `${file}: no bare login EmptyState`);
  }
  // Tap handlers go through useMemberAction (see the M1 test below).
});

test("no screen picks login copy or a login prompt from `!user` (class scan over app/ and components/)", () => {
  const walk = dir => fs.readdirSync(path.join(root, dir), { withFileTypes: true }).flatMap(entry => entry.isDirectory() ? walk(path.join(dir, entry.name)) : /\.tsx?$/.test(entry.name) ? [path.join(dir, entry.name)] : []);
  const patterns = [
    /\buser \?[^:;]{0,80}:\s*["'][^"']*로그인/,                                   // user ? "…" : "로그인하기"
    /!user \?\s*<EmptyState/,                                                     // !user ? <EmptyState 로그인 …>
    /!user \?\s*["'][^"']*로그인/,                                                // !user ? "로그인…" : …
    /!user\)\s*(router\.push\(["']\/login|login\.prompt|onGuest|requireLogin)/, // if (!user) prompt
    /else\s+(login\.prompt|requireLogin|onGuest)\(/,                              // if (user) … else prompt
    /:\s*requireLogin\(/,                                                          // user ? … : requireLogin(
  ];
  // Screens with a members-only query and an empty state must route non-members through NonMemberState.
  const publicOrRedirecting = new Set(["app/form-fill.tsx", "app/profile-edit.tsx", "app/programs/[id].tsx"]);
  // 홈 is public; only its 신청 이력 query (the "이어서 진행" title variant) is members-only, so only that line is set aside.
  const setAside = { "app/(tabs)/index.tsx": /^.*useQuery\(\{ queryKey: \["applications", user\?\.id\][^\n]*$/m };
  const offenders = [];
  for (const file of [...walk("app"), ...walk("components")]) {
    const source = setAside[file] ? read(file).replace(setAside[file], "") : read(file);
    if (setAside[file]) assert.notEqual(source, read(file), `${file}: the set-aside query line moved`);
    for (const pattern of patterns) if (pattern.test(source)) offenders.push(`${file}: ${pattern}`);
    if (/enabled: !!user/.test(source) && /<EmptyState/.test(source) && !/NonMemberState/.test(source) && !publicOrRedirecting.has(file)) offenders.push(`${file}: members-only screen without NonMemberState`);
  }
  assert.deepEqual(offenders, []);
});

test("a members-only tap while the session restores is kept and replayed through the latest handler, never dropped or a login prompt (M1, L1-L3)", () => {
  const load = loader({
    "@react-native-async-storage/async-storage": { default: {}, __esModule: true },
    "expo-router": { useRouter: () => ({ push() {} }) },
    "@/store/auth": { useAuthStore: () => null },
    "./navigation": { requireLogin() {} },
  });
  const { memberTap, settleQueuedTap, tapStep, replayStep } = load("lib/guest.ts");
  assert.deepEqual(["member", "guest", "pending", "pending-error"].map(memberTap), ["run", "guest", "queue", "unconfirmed"]);
  assert.equal(settleQueuedTap("pending"), null);
  // A tap: a member whose data is not loaded yet is kept too; a second tap while one is kept is ignored (L2).
  assert.equal(tapStep("member", true, false), "run");
  assert.equal(tapStep("member", false, false), "queue", "the data the action decides on is still loading");
  assert.equal(tapStep("pending", true, false), "queue");
  assert.equal(tapStep("guest", true, false), "guest");
  assert.equal(tapStep("pending-error", true, false), "unconfirmed");
  for (const state of ["member", "pending", "guest", "pending-error"]) assert.equal(tapStep(state, true, true), "ignore", state);
  // A replay waits for the member AND the data.
  assert.equal(replayStep("pending", true), null);
  assert.equal(replayStep("member", false), null, "member arrived, data still loading: keep waiting");
  assert.equal(replayStep("member", true), "run");
  assert.equal(replayStep("guest", false), "guest", "the restore ended logged out: the nudge, whatever the data");
  assert.equal(replayStep("pending-error", false), "unconfirmed");
  const hook = read("components/MemberAction.tsx");
  assert.match(hook, /latest\.current = handlers;/, "replays read the latest handler (L3)");
  assert.match(hook, /latest\.current\[next\.key\]\?\.run\(next\.intent\)/);
  assert.match(hook, /useFocusEffect\(useCallback\(\(\) => clear, \[clear\]\)\)/, "leaving the screen drops a kept tap (L1)");
  assert.match(hook, /PENDING_ERROR_TOAST = "연결을 확인하고 있어요\. 연결되면 다시 눌러 주세요\."/);
  // Every members-only tap goes through it, with the live handler and a readiness gate where the action depends on data.
  for (const [file, patterns] of [
    ["app/programs/[id].tsx", [/loading=\{bookmark\.isPending \|\| member\.waiting === 'save'\}/, /loading=\{alerts\.isPending \|\| member\.waiting === 'alert'\}/, /router\.push\(benefitProfile \? '\/profile-edit' : '\/onboarding\?mode=edit'\);\n\s*\}, ready: profileStatus\.state !== 'loading' \}/]],
    ["components/MyDocumentItems.tsx", [/loading=\{member\.waiting === "add"\} onPress=\{\(\) => member\.run\("add", onGuest\)\}/]],
    ["app/document-vault.tsx", [/useMemberAction\(\{ add: \{ run: \(\) => setShowAddModal\(true\) \} \}\)/]],
    ["app/(tabs)/index.tsx", [/reco: \{ run: goRecommendation, ready: profileStatus\.state !== "loading"/, /\{member\.toastElement\}/]],
    ["app/(tabs)/explore.tsx", [/local: \{ run: \(\) => selectTarget\("local"\), ready: profileStatus\.state !== "loading" \}/, /\{member\.toastElement\}/]],
    ["components/RecommendationList.tsx", [/conditions: \{ run: \(\) => openConditions\(\), ready: profileStatus\.state !== "loading" \}/, /\{member\.toastElement\}/]],
  ]) for (const pattern of patterns) assert.match(read(file), pattern, `${file}: ${pattern}`);
});

test("a kept 찜 never deletes a bookmark: the toggle runs only after the saved list loads and only if not already saved (N-H1)", () => {
  const load = loader({
    react: { default: {}, __esModule: true, useCallback: f => f, useEffect() {}, useRef: v => ({ current: v }), useState: v => [v, () => {}] },
    "react/jsx-runtime": { jsx: (type, props) => ({ type, props }), jsxs: (type, props) => ({ type, props }) },
    "expo-router": { useFocusEffect() {} },
    "./UndoToast": { default: "UndoToast", useUndoToast: () => ({ toast: null, show() {}, hide() {} }), __esModule: true },
    "@/lib/guest": {},
  }, { "@/lib/tapHold": "lib/tapHold.ts" });
  const { saveDecision, ALREADY_SAVED_TOAST, SAVED_UNKNOWN_TOAST, HOLD_LIMIT_MS, BUSY_TOAST } = load("components/MemberAction.tsx");
  // M1: a failed saved list means the bookmark state is unknown; the toggle is unreachable, whatever the intent.
  for (const want of [true, false]) for (const isSaved of [true, false]) assert.equal(saveDecision(want, isSaved, true), "retry", `want=${want} isSaved=${isSaved}`);
  assert.equal(SAVED_UNKNOWN_TOAST, "관심 혜택 상태를 확인하지 못했어요. 잠시 후 다시 눌러 주세요.");
  // M2: a kept tap is dropped after HOLD_LIMIT_MS with the pending toast; an ignored tap says why.
  assert.equal(HOLD_LIMIT_MS, 10_000);
  assert.equal(BUSY_TOAST, "앞에서 누른 동작을 처리하고 있어요.");
  const hook = read("components/MemberAction.tsx");
  assert.match(hook, /createTapHold\(setWaiting, \(\) => onExpire\.current\(\)\)/, "the hook holds taps through createTapHold (tested as a function below)");
  assert.match(hook, /if \(step === "ignore"\) say\(BUSY_TOAST\)/);
  assert.equal(saveDecision(true, false), "toggle", "empty icon, not saved: save it");
  assert.equal(saveDecision(true, true), "already-saved", "kept while the icon looked empty, but it was saved: do not toggle it off");
  assert.equal(saveDecision(false, true), "toggle", "a member unsaving");
  assert.equal(saveDecision(false, false), "already-unsaved");
  assert.equal(ALREADY_SAVED_TOAST, "이미 관심 혜택에 있어요");
  const detail = read("app/programs/[id].tsx");
  // The only path to toggleBookmark is the guarded one.
  assert.equal((detail.match(/bookmark\.mutate\(/g) ?? []).length, 1, "one call site");
  assert.match(detail, /mutationFn: \(want: boolean\) => setBookmarkToIntent\(\{\n\s*put: bookmarked => api\.setBookmark\(id, bookmarked\),/, "the intent travels with the request as an idempotent PUT (H1)");
  assert.match(detail, /if \(result\.bookmarked !== want\) \{ toast\.show\(\{ text: SAVED_UNKNOWN_TOAST \}\);/, "ALREADY_* only when the final state is the intent");
  assert.equal((detail.match(/api\.toggleBookmark\(/g) ?? []).length, 1, "the toggle is only reached through toggleToIntent");
  assert.match(detail, /const decision = saveDecision\(want === true, !!isSaved, saved\.isError\);\n\s*if \(decision === 'toggle'\) bookmark\.mutate\(want === true\);/, "the toggle only on a loaded list (an error gives retry)");
  assert.match(detail, /else if \(decision === 'retry'\) \{ toast\.show\(\{ text: SAVED_UNKNOWN_TOAST \}\); void saved\.refetch\(\); \}/);
  // isFetched is also true after an error; the gate is success or error, and error never toggles.
  assert.match(detail, /save: \{ run: saveOnce, ready: saved\.isSuccess \|\| saved\.isError \}/, "a kept 찜 waits for the saved list to answer");
  assert.doesNotMatch(detail, /ready: saved\.isFetched/);
  // L-a: the icon follows the server's answer at once (savedAfterToggle is tested as a function below).
  assert.match(detail, /client\.setQueryData<\{ items: SavedProgram\[\] \}>\(\['saved', user\?\.id\], old => savedAfterToggle\(old, id, row, result\.bookmarked\)\)/);
  // The bookmark write's keys (the saved list among them) live in lib/writeEffects.ts; check-write-effects.cjs pins them.
  assert.match(detail, /onError: \(e: Error\) => \{[\s\S]{0,200}invalidateAfterWrite\(client, 'bookmark'/, "a failed request re-reads the saved list");
  assert.match(read("app/(tabs)/agent.tsx"), /invalidateAfterApproval\(client, \{ run, decided, confirmationId \}, userId\)/);
  assert.match(detail, /member\.run\('save', \(\) => login\.prompt\(GUEST_SAVE_REASON\), !isSaved\)/, "the tap carries its intent");
});

test("a failed profile read is 'try again', never 'no profile' (L-e)", () => {
  const load = loader({
    "@tanstack/react-query": { useQuery() {} },
    "./api": { api: {}, ApiRequestError: class ApiRequestError extends Error { constructor(m, s) { super(m); this.status = s; } } },
    "@/store/onboarding": { useOnboardingStore() {}, getBirthYear() {} },
    "@/store/auth": { useAuthStore() {} },
    "@/constants/regions": { cityCodeFor() {}, findRegion() {} },
    "./profileCodes": {}, "./memberProfile": {}, "./optionalAnswers": {},
  });
  const mod = load("lib/useBenefitProfile.ts");
  assert.equal(mod.profileStateOf({ isSuccess: true, isError: false, data: { profile: { region_code: "busan" } } }), "found");
  assert.equal(mod.profileStateOf({ isSuccess: true, isError: false, data: { profile: null } }), "none");
  assert.equal(mod.profileStateOf({ isSuccess: false, isError: false }), "loading");
  assert.equal(mod.profileStateOf({ isSuccess: false, isError: true, error: new Error("network") }), "error", "a network or 5xx failure");
  assert.equal(mod.profileStateOf({ isSuccess: false, isError: true, error: new Error("network"), data: { profile: { region_code: "busan" } } }), "found", "a failed background refetch over loaded conditions (L-2)");
  assert.equal(mod.PROFILE_RETRY_TOAST, "저장한 조건을 불러오지 못했어요. 잠시 후 다시 눌러 주세요.");
  for (const [file, pattern] of [
    ["app/programs/[id].tsx", /if \(profileStatus\.state === 'error'\) \{ toast\.show\(\{ text: PROFILE_RETRY_TOAST \}\); profileStatus\.retry\(\); return; \}/],
    ["components/RecommendationList.tsx", /if \(profileStatus\.state === "error"\) \{ member\.say\(PROFILE_RETRY_TOAST\); profileStatus\.retry\(\); return; \}/],
    ["app/(tabs)/index.tsx", /if \(profileStatus\.state === "error"\) \{ member\.say\(PROFILE_RETRY_TOAST\); profileStatus\.retry\(\); return; \}/],
    ["app/(tabs)/explore.tsx", /if \(profileStatus\.state === "error"\) \{ member\.say\(PROFILE_RETRY_TOAST\); profileStatus\.retry\(\); return; \}/],
  ]) assert.match(read(file), pattern, file);
});

test("a save tap never deletes a bookmark: idempotent PUT with safe retries, and a read before any second toggle on old servers (H1, review 7)", async () => {
  const { setBookmarkToIntent, savedAfterToggle, ALREADY_UNSAVED_TOAST, isMissingPut } = loader({})("lib/bookmarkSync.ts");
  const networkError = () => Object.assign(new Error("연결을 확인한 뒤 다시 시도해 주세요."), { status: 0 });
  // An in-memory server. `lose` drops the response of the given call numbers after the server committed.
  function server(saved, { put = true, lose = [] } = {}) {
    const calls = [];
    const respond = (kind, value) => { calls.push(kind); if (lose.includes(calls.length)) throw networkError(); return { bookmarked: value }; };
    return {
      calls, state: () => saved,
      transport: {
        put: async bookmarked => { if (!put) throw Object.assign(new Error("Method Not Allowed"), { status: 405 }); saved = bookmarked; return respond("put", saved); },
        toggle: async () => { saved = !saved; return respond("toggle", saved); },
        isSaved: async () => { calls.push("read"); return saved; },
      },
    };
  }
  // A response lost after the server committed: the PUT is simply sent again; the bookmark stays.
  let s = server(false, { lose: [1] });
  assert.deepEqual(await setBookmarkToIntent(s.transport, true), { bookmarked: true, corrected: false });
  assert.equal(s.state(), true);
  assert.deepEqual(s.calls, ["put", "put"]);
  // Stale cache, the server already had it: PUT is a no-op, never a delete.
  s = server(true);
  assert.deepEqual(await setBookmarkToIntent(s.transport, true), { bookmarked: true, corrected: false });
  assert.equal(s.state(), true);
  // At most 2 retries, then the error surfaces (the screen invalidates and re-reads).
  s = server(false, { lose: [1, 2, 3] });
  await assert.rejects(setBookmarkToIntent(s.transport, true), error => error.status === 0);
  assert.equal(s.calls.length, 3);
  assert.equal(s.state(), true, "every attempt set the intent; none removed it");
  // A non-network error is not retried.
  const failing = { put: async () => { throw Object.assign(new Error("x"), { status: 503 }); }, toggle: async () => assert.fail("no toggle"), isSaved: async () => assert.fail("no read") };
  await assert.rejects(setBookmarkToIntent(failing, true), error => error.status === 503);
  // The new server's own 404 (unknown programme) is not "PUT missing".
  assert.equal(isMissingPut({ status: 404, message: "혜택을 찾을 수 없어요." }), false);
  assert.equal(isMissingPut({ status: 405 }), true);
  assert.equal(isMissingPut({ status: 404, message: "요청한 정보를 찾을 수 없어요." }), true);
  // Old server (POST only): the toggle answer matches: one call.
  s = server(false, { put: false });
  assert.deepEqual(await setBookmarkToIntent(s.transport, true), { bookmarked: true, corrected: false });
  assert.deepEqual(s.calls, ["toggle"]);
  // Old server, stale cache (it was saved): the toggle removed it; a read shows "not saved", so one more toggle restores it.
  s = server(true, { put: false });
  assert.deepEqual(await setBookmarkToIntent(s.transport, true), { bookmarked: true, corrected: true });
  assert.equal(s.state(), true);
  assert.deepEqual(s.calls, ["toggle", "read", "toggle"]);
  // Old server: the first toggle's response was lost after commit, the screen retried later and the read
  // shows the intent already holds: no second toggle.
  s = server(false, { put: false });
  const already = { ...s.transport, toggle: async () => ({ bookmarked: false }), isSaved: async () => true };
  assert.deepEqual(await setBookmarkToIntent(already, true), { bookmarked: true, corrected: true }, "read first, no blind toggle");
  assert.equal(ALREADY_UNSAVED_TOAST, "이미 관심 혜택에서 빠져 있어요");
  // savedAfterToggle: the cache follows the server's last answer (L-3, as a function).
  const a = { id: "a" }, b = { id: "b" }, x = { id: "x" };
  assert.equal(savedAfterToggle(undefined, "x", x, true), undefined, "no list loaded: left alone");
  assert.deepEqual(savedAfterToggle({ items: [a, b], total: 2 }, "x", x, true), { items: [x, a, b], total: 2 }, "saved: newest first, other fields kept");
  assert.deepEqual(savedAfterToggle({ items: [x, a] }, "x", x, true).items, [x, a], "already listed: not duplicated");
  assert.deepEqual(savedAfterToggle({ items: [a, x, b] }, "x", x, false).items, [a, b], "unsaved: removed");
  assert.deepEqual(savedAfterToggle({ items: [a] }, "x", undefined, true).items, [a], "no programme to insert: list unchanged except removal");
});

test("the kept tap: one at a time, dropped after 10 s with the pending toast, replayed at most once (L-3, as functions)", (t) => {
  const { createTapHold, HOLD_LIMIT_MS } = loader({})("lib/tapHold.ts");
  assert.equal(HOLD_LIMIT_MS, 10_000);
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const changes = [], expired = [];
  let hold = createTapHold(key => changes.push(key), () => expired.push(true));
  const tap = key => ({ key, intent: true, onGuest() {} });
  assert.equal(hold.keep(tap("save")), true);
  assert.equal(hold.keep(tap("alert")), false, "a second tap is refused while one is kept (the hook shows BUSY_TOAST)");
  assert.equal(hold.current().key, "save");
  t.mock.timers.tick(9_999);
  assert.deepEqual(expired, [], "not yet");
  t.mock.timers.tick(1);
  assert.deepEqual(expired, [true], "dropped after the limit, the hook shows PENDING_ERROR_TOAST");
  assert.equal(hold.current(), null);
  assert.deepEqual(changes, ["save", null], "the spinner starts and stops");
  // A replay takes the tap and stops the timer: it runs once, never also as an expiry.
  hold = createTapHold(() => {}, () => expired.push("late"));
  hold.keep(tap("save"));
  assert.equal(hold.take().key, "save");
  assert.equal(hold.take(), null, "taken once");
  t.mock.timers.tick(20_000);
  assert.deepEqual(expired, [true], "no expiry after a replay");
  // Leaving the screen clears it without an expiry.
  hold.keep(tap("add")); hold.clear(); t.mock.timers.tick(20_000);
  assert.deepEqual(expired, [true]);
  t.mock.timers.reset();
});
