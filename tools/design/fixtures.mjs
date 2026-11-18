// Deterministic API fixtures and a static server for the browser checks. No request leaves the machine.
import fs from "node:fs";
import path from "node:path";
import http from "node:http";

const mime = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".png": "image/png", ".otf": "font/otf", ".ttf": "font/ttf", ".json": "application/json" };

export async function serve(web) {
  const server = http.createServer((req, res) => {
    const local = path.resolve(web, "." + decodeURIComponent(new URL(req.url, "http://localhost").pathname));
    if (!local.startsWith(web + path.sep) && local !== web) { res.writeHead(403).end(); return; }
    const file = [local, local + ".html", path.join(local, "index.html"), path.join(web, "index.html")].find(p => fs.existsSync(p) && fs.statSync(p).isFile());
    res.setHeader("Content-Type", mime[path.extname(file)] ?? "application/octet-stream");
    fs.createReadStream(file).pipe(res);
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  return { base: `http://127.0.0.1:${server.address().port}`, close: () => new Promise(resolve => server.close(resolve)) };
}

const day = 86400000;
export const programs = [
  { id: "11111111-1111-4111-8111-111111111111", title: "부산 청년 월세 지원", program_type: "policy", provider_name: "부산광역시", summary: "청년의 주거비 부담을 덜어주는 월세 지원 혜택입니다.", days: 4 },
  { id: "22222222-2222-4222-8222-222222222222", title: "한국장학재단 국가장학금", program_type: "scholarship", provider_name: "한국장학재단", summary: "소득과 학적 조건에 따라 대학생의 등록금을 지원합니다.", days: 7 },
  { id: "33333333-3333-4333-8333-333333333333", title: "대학생 소프트웨어 이용 혜택", program_type: "corporate_benefit", provider_name: "테스트 기업", summary: "대학생이 사용할 수 있는 소프트웨어 혜택을 확인해 보세요.", days: 9 },
].map(({ days, ...p }) => ({ ...p, program_status: "active", application_end_at: new Date(Date.now() + days * day).toISOString(), deadline_at: new Date(Date.now() + days * day).toISOString(), benefit_amount_monthly: 200000, benefit_amount_semester: null, regions: ["busan"], min_age: 19, max_age: 39, required_documents: [], application_method: "공식 홈페이지에서 온라인 신청", official_url: "https://example.org/notice", application_policy: { can_prepare: true, reason: "", destination: { url: "https://example.org/notice", label: "공식 신청 안내" }, workflow: { no_application_required: false, channels: [], needs_confirmation: false } } }));
// Vault entries as the app stores them on the device (metadata only, no file).
const VAULT_DOCUMENTS = [
  { id: "doc-1", type: "enrollment_cert", name: "재학증명서", fileUri: null, mimeType: null, issuedAt: new Date(Date.now() - 20 * day).toISOString(), expiresAt: null, addedAt: new Date().toISOString(), note: null, encrypted: true },
  { id: "doc-2", type: "resident_register", name: "주민등록등본", fileUri: "file:///fixture/doc-2.enc", mimeType: "application/pdf", issuedAt: new Date(Date.now() - 5 * day).toISOString(), expiresAt: null, addedAt: new Date().toISOString(), note: null, encrypted: true },
];

/** A programme id the fixture answers with 404 (closed or retired). */
export const MISSING_PROGRAM = "00000000-0000-4000-8000-000000000404";
export const profile = { nickname: "마지미", birth_year: 2002, region_code: "busan", enrollment_status: "재학", school_name: "부산대학교", income_bracket: 3, employment_status: "미취업" };
export const consent = { required: { termsAgreed: true, privacyAgreed: true, agreedAt: new Date().toISOString(), termsVersion: "2026-03-21", privacyVersion: "2026-09-26" }, healthInsurance: { agreed: false } };

// `agent`: the server's 비서 switch (GET /assistant/agent, lib/api.ts getAgentStatus).
export async function context(browser, { signedIn = false, consented = signedIn, width = 390, height = 844, errors = [], agent = true } = {}) {
  const c = await browser.newContext({ viewport: { width, height }, deviceScaleFactor: 1 });
  await c.addInitScript(({ consent, signedIn, consented, documents }) => {
    if (!location.protocol.startsWith("http")) return;
    if (consented) localStorage.setItem("mazimi_consent_v1", JSON.stringify(consent));
    if (signedIn) sessionStorage.setItem("mazimi_auth_token", "local-ui-fixture-only");
    if (signedIn) localStorage.setItem("document_vault_v2:fixture-user", JSON.stringify(documents));
  }, { consent, signedIn, consented, documents: VAULT_DOCUMENTS });
  const stages = states => [["discover", "발견"], ["eligibility", "자격 확인"], ["documents", "서류 준비"], ["apply", "신청"]].map(([stage, label], i) => ({ stage, label, state: states[i] }));
  await c.route("**/api/v1/**", async route => {
    const u = new URL(route.request().url());
    const p = u.pathname.replace("/api/v1", "");
    const headers = { "access-control-allow-origin": "*", "access-control-allow-headers": "content-type,authorization", "access-control-allow-methods": "GET,POST,PUT,DELETE,OPTIONS" };
    if (route.request().method() === "OPTIONS") return route.fulfill({ status: 204, headers });
    let json;
    if (p === "/auth/providers") json = { kakao: true, google: false };
    else if (p === "/auth/me") json = { id: "fixture-user", nickname: "마지미", image: null };
    else if (p === "/profile") json = { profile, completeness: 80 };
    else if (p === "/recommend/preview" || p.startsWith("/recommendations")) json = { total_available: 3, estimated_monthly: 200000, estimated_semester: 0, items: programs.map(item => ({ program_id: item.id, title: item.title, program_type: item.program_type, provider_name: item.provider_name, summary: item.summary, deadline: item.deadline_at, reasons: [item.summary], missing_checks: [], eligibility_status: "unknown", match_score: 80, benefit_amount_monthly: 200000, benefit_amount_semester: null })) };
    else if (p === "/programs" || p === "/campus/programs") json = { items: programs, total: programs.length, has_next: false, page: 1 };
    else if (p === "/campus/universities") json = { items: [{ id: "pnu", name: "부산대학교", aliases: ["부산대"], region: "busan" }] };
    else if (p.startsWith("/assistant/bundles")) json = { version: 1, scenario: u.searchParams.get("scenario") ?? "returning_student", profile_version: 1, required_profile_fields: [], notice: "공식 공고에서 최종 조건을 확인해 주세요.", groups: [{ title: "학교생활·복학", basis: "내 조건 기준", total_candidates: 2, items: programs.slice(0, 2).map(item => ({ program_id: item.id, title: item.title, program_type: item.program_type, provider_name: item.provider_name, summary: item.summary, deadline: item.deadline_at, reasons: [item.summary], missing_checks: [], eligibility_status: "unknown", match_score: 80 })) }] };
    else if (/^\/programs\/[^/]+\/assistant$/.test(p)) { const item = programs[0]; json = { version: 1, program_id: item.id, title: item.title, amounts: { monthly: 200000, semester: null, once: null }, mode: "rules", ai_available: false, notice: "규칙으로 정리한 안내예요.", eligibility: { status: "unknown", summary: "연령과 지역은 맞아요. 소득은 확인이 필요해요.", checks: [{ label: "연령", status: "pass", detail: "만 19세 이상" }, { label: "소득", status: "unknown", detail: "중위소득 60% 이하" }], missing_fields: [], profile_version: 1 }, preparation: { revision: "r1", title: item.title, policy: item.application_policy, tasks: [{ id: "t1", title: "신청서 작성", detail: "공식 양식", kind: "document" }], completed_tasks: [], progress_reset: false, selected_channel: null, official_links: [] }, remaining_task_ids: ["t1"], alert_enabled: false, deadline_at: item.deadline_at, source: { official_url: item.official_url, last_synced_at: null, program_updated_at: new Date().toISOString(), checked_at: new Date().toISOString(), note: "" }, sections: [{ topic: "summary", title: "지원 내용", body: item.summary, basis: "공고" }] }; }
    else if (p === `/programs/${MISSING_PROGRAM}`) return route.fulfill({ status: 404, contentType: "application/json", headers, body: JSON.stringify({ error: "Program not found" }) });
    // 신청 진행 단계 (server application_procedure): a member's own stages, drawn as the Figma frame 34065:10997 shows them.
    else if (/^\/programs\/[^/]+\/progress$/.test(p)) json = { stages: stages(["done", "current", "upcoming", "upcoming"]) };
    else if (/^\/programs\/[^/]+$/.test(p)) json = { application_progress: stages(["current", "upcoming", "upcoming", "upcoming"]), ...(programs.find(item => item.id === p.split("/").pop()) ?? programs[0]) };
    else if (p.includes("/saved")) json = { items: programs.map(item => ({ ...item, program_id: item.id, saved_at: new Date().toISOString() })) };
    else if (p === "/alerts") json = { total: 3, limit: 50, offset: 0, unread: 1, items: programs.map((item, i) => ({ id: `alert-${i}`, user_id: "fixture-user", program_id: item.id, alert_type: "deadline", alert_date: new Date(Date.now() - i * day).toISOString(), program_title: item.title, official_url: item.official_url, deadline_at: item.deadline_at, application_end_at: item.application_end_at, read_at: i === 0 ? null : new Date().toISOString() })) };
    else if (p === "/my/applications") json = { total: 3, items: [["applied", "신청 완료"], ["planning", "준비 중"], ["abandoned", "신청 안 함"]].map(([status, label], i) => ({ program_id: programs[i].id, program_title: programs[i].title, current_status: status, display_status: status, display_label: label, memo: i === 0 ? "접수 번호 2026-0001" : null, applied_at: null, result_at: null, updated_at: new Date(Date.now() - i * day).toISOString(), history: [] })) };
    else if (p.includes("notification-preferences")) json = { deadline: true, new_program: false, profile_update: true, channels: { in_app: true, push: false, kakao: false, email: false } };
    else if (p === "/assistant/agent") json = { enabled: agent, model_planner_available: false, retention_days: 30, run_timeout_secs: 60, goals: [] };
    else json = { items: [], total: 0 };
    await route.fulfill({ status: 200, contentType: "application/json", headers, body: JSON.stringify(json) });
  });
  c.on("page", page => page.on("pageerror", e => errors.push(e.message)));
  return c;
}

export async function settle(page) {
  await page.waitForTimeout(700);
  await page.evaluate(() => document.fonts.ready);
}
