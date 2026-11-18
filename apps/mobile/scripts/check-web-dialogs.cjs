// react-native-web's Alert.alert is an empty function, so any notice that must be
// visible on the web build has to go through lib/notify.ts (window.alert/confirm).
// Unit-tests the helper and openApplyUrl on both platforms, then scans the screens
// the 2026-10-23 QA found silent on web.
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");

const root = path.resolve(__dirname, "..");
const read = relative => fs.readFileSync(path.join(root, relative), "utf8");

function load(relative, rn) {
  const source = ts.transpileModule(read(relative), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  const dependency = name => {
    if (name === "react-native") return rn;
    if (name === "./notify") return load("lib/notify.ts", rn);
    throw new Error(`Unmocked dependency ${name}`);
  };
  new Function("require", "module", "exports", source)(dependency, module, module.exports);
  return module.exports;
}

function platform(os, { confirm = true } = {}) {
  const calls = { alert: [], windowAlert: [], windowConfirm: [], opened: [] };
  global.window = { alert: m => calls.windowAlert.push(m), confirm: m => { calls.windowConfirm.push(m); return confirm; } };
  const rn = {
    Platform: { OS: os },
    Alert: { alert: (...args) => calls.alert.push(args) },
    Linking: { canOpenURL: async () => true, openURL: async url => { calls.opened.push(url); } },
  };
  return { rn, calls };
}

test("notify uses window.alert on web and Alert.alert on native", () => {
  let { rn, calls } = platform("web");
  load("lib/notify.ts", rn).notify("저장 실패", "다시 시도해 주세요.");
  assert.deepEqual(calls.windowAlert, ["저장 실패\n다시 시도해 주세요."]);
  assert.equal(calls.alert.length, 0);

  ({ rn, calls } = platform("android"));
  load("lib/notify.ts", rn).notify("저장 실패", "다시 시도해 주세요.");
  assert.deepEqual(calls.alert, [["저장 실패", "다시 시도해 주세요."]]);
  assert.equal(calls.windowAlert.length, 0);
});

test("confirmAction runs the action only when the web user confirms", () => {
  let ran = 0;
  let { rn, calls } = platform("web", { confirm: false });
  load("lib/notify.ts", rn).confirmAction("이동", "열까요?", "열기", () => ran++);
  assert.equal(ran, 0); assert.equal(calls.windowConfirm.length, 1);
  ({ rn } = platform("web", { confirm: true }));
  load("lib/notify.ts", rn).confirmAction("이동", "열까요?", "열기", () => ran++);
  assert.equal(ran, 1);
});

test("openApplyUrl asks with window.confirm on web and opens the URL", async () => {
  const { rn, calls } = platform("web");
  load("lib/openExternalUrl.ts", rn).openApplyUrl("https://www.gov.kr/x");
  await new Promise(r => setImmediate(r));
  assert.equal(calls.windowConfirm.length, 1);
  assert.match(calls.windowConfirm[0], /www\.gov\.kr/);
  assert.deepEqual(calls.opened, ["https://www.gov.kr/x"]);
  assert.equal(calls.alert.length, 0);
});

test("screens that must speak on web do not call Alert.alert directly", () => {
  for (const file of ["app/(tabs)/profile.tsx", "app/(tabs)/manage.tsx", "app/account.tsx", "lib/openExternalUrl.ts"]) {
    assert.doesNotMatch(read(file), /Alert\.alert\(/, file);
  }
  // document-vault keeps Alert.alert only inside native-only branches.
  const vault = read("app/document-vault.tsx");
  const titles = [...vault.matchAll(/Alert\.alert\(\s*"([^"]+)"/g)].map(m => m[1]);
  const nativeOnly = new Set(["카메라 권한 필요", "사진 라이브러리 권한 필요", "서류 추가 방법"]);
  assert.deepEqual(titles.filter(t => !nativeOnly.has(t)), [], "Alert.alert outside native-only branches");
});

test("the web vault shows the keep-in-app notice instead of a dead attach button", () => {
  const vault = read("app/document-vault.tsx");
  assert.match(vault, /파일은 마지미 앱에서 보관할 수 있어요/);
  assert.doesNotMatch(vault, /"앱에서 파일 보관"/, "the silent web Alert is gone");
});

test("the Android add-options dialog fits three buttons and stays cancelable", () => {
  const vault = read("app/document-vault.tsx");
  const call = vault.slice(vault.indexOf('Alert.alert("서류 추가 방법"'));
  const body = call.slice(0, call.indexOf("]") + 40);
  assert.equal((body.match(/text:/g) ?? []).length, 3, body);
  assert.match(body, /cancelable:\s*true/);
});

test("scan preview and 건보 consent modals render inside the AddDoc modal tree (iOS presents only from the top sheet)", () => {
  const vault = read("app/document-vault.tsx");
  const start = vault.indexOf("function AddDocModal(");
  const body = vault.slice(start, vault.indexOf("\nconst modalStyles", start));
  const sheetOpen = body.indexOf("<Modal\n        visible={visible}");
  assert.ok(sheetOpen > 0, "AddDoc <Modal> found");
  const sheetClose = body.lastIndexOf("</Modal>");
  for (const child of ["<ScanPreviewModal", "<HealthInsuranceConsentModal"]) {
    const at = body.indexOf(child);
    assert.ok(at > sheetOpen && at < sheetClose, `${child} is inside the AddDoc modal`);
  }
});

test("iOS retake reopens the camera only after the scan preview has dismissed", () => {
  const vault = read("app/document-vault.tsx");
  const retake = vault.slice(vault.indexOf("function handleScanRetake()"), vault.indexOf("async function handleGallery()"));
  assert.match(retake, /Platform\.OS === "ios"/);
  assert.match(retake, /cameraAfterScan\.current = true/);
  assert.match(vault, /onDismiss=\{handleScanDismiss\}/);
  assert.doesNotMatch(vault.slice(vault.indexOf("function ScanPreviewModal("), vault.indexOf("function ScanPreviewModal(") + 2500), /if \(!rawUri\) return null;/, "the modal stays mounted so onDismiss fires");
});

test("the web vault links to one install URL constant, never the app scheme", () => {
  const vault = read("app/document-vault.tsx");
  assert.doesNotMatch(vault, /mazimi:\/\//);
  assert.match(vault, /APP_INSTALL_URL/);
  assert.match(read("constants/links.ts"), /export const APP_INSTALL_URL/);
});

test("the dead DocumentCard is gone and the 서류 삭제 confirm is the .fig pop-up (a Modal, so it works on web)", () => {
  const vault = read("app/document-vault.tsx");
  assert.doesNotMatch(vault, /function DocumentCard\(/);
  assert.match(vault, /confirm\.ask\(\{title:"서류 삭제"/);
  assert.match(vault, /\{confirm\.dialog\}/, "the dialog is mounted");
  assert.match(read("components/ConfirmDialog.tsx"), /<Modal visible=\{request !== null\}/);
});

test("the session banner sits below the header and can be dismissed", () => {
  const banner = read("components/SessionProblem.tsx");
  assert.match(banner, /top: insets\.top \+ 64/);
  assert.match(banner, /accessibilityLabel="연결 안내 닫기"/);
});
